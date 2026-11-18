# The cauldron schema: `cauldron` grammar v1

A cauldron's soup holds two pools. The first, `ws`, is the knowledge graph
itself, on the `workspace` grammar v6 (`../workspace-v6/README.md`): typed
nodes in overlapping **regions**. The second, `cauldron`, sits above it and
holds **clusters**, on the grammar this directory publishes, kind `cauldron`,
version `1`.

Version: `cauldron@1`. `workspace@6` is unchanged by it: a workspace record
reads and writes exactly as before.

## What a cluster is

A **cluster** is an area of the surface: a name and a boundary description
saying what it covers. It **claims** regions of the workspace pool: a
`claims` edge from the cluster to a region. Any cluster may claim any region,
and a region may be claimed by many clusters.

How it differs from a region:

| | region (`R-<n>`, pool `ws`) | cluster (`CL-<n>`, pool `cauldron`) |
|---|---|---|
| holds | nodes: a node sits in a region with an `in_region` edge | regions: a cluster claims a region with a `claims` edge |
| nests | yes, with `narrows` | no |
| written by | sessions and the owner; a ripple writes into regions | sessions and the owner; a ripple never writes a cluster or a claim in this release |
| the unit of | knowledge (what belongs together) | the surface (which areas of the soup a view or a person works across) |

A claim is a statement about the surface. It is **not** a lease: it holds
nothing, assigns nothing to anyone, and never expires.

## Files

| file | what it is |
|---|---|
| `grammar.json` | the grammar itself, exactly as the soup pins it (the serialized `PoolGrammar` for the pool `cauldron`) |
| `node.cluster.schema.json` | the JSON Schema (draft 2020-12) of a cluster record at mint |
| `remint.cluster.schema.json` | the same, for a record of a `--remint` batch (`id` a batch label; an edge's `to` may name a label of the batch) |
| `edges.json` | the one edge kind, `claims` |
| `README.md` | this file |

Every `.json` file here is **generated** from the Rust grammar
(`libs/spine/src/grammar_cauldron.rs`, rendered by
`libs/spine/src/schema_publish.rs`, the same generator as `workspace-v6`) and
never edited by hand. A test in the spine crate fails when they drift. To
regenerate:

```sh
SPINE_SCHEMA_BLESS=1 cargo test -p spine --lib schema_publish
```

## The record

```json
{
  "id": "CL-1",
  "type": "cluster",
  "handle": "Release engineering",
  "boundary": "Inside: building, signing, packaging and publishing releases. Outside: what a release contains.",
  "meta": {"cites": [{"kind": "url", "url": "https://example.com/plan"}]},
  "edges": [
    {"kind": "claims", "to": "R-3"},
    {"kind": "claims", "to": "R-7", "note": "only its install half"}
  ]
}
```

| field | rule |
|---|---|
| `id` | `CL-<n>`; with `--remint` (or `remint: true`), a batch label |
| `type` | `cluster` |
| `handle` | required, 1 to 235 characters |
| `boundary` | required, 40 to 1,000 characters: the same rule as a region's boundary. Write what is inside and what is outside, as for a region |
| `meta` | optional, an open object. `meta.cites` is checked against the citation table of `workspace-v6/README.md` ("Conventions"), as on every record |
| `state` | none: a cluster carries no state, and a record with one is refused |

### `claims`

- From a `cluster` to a `region` in a pool of kind `workspace` (the soup's
  `ws`). No declared fields: an edge carries `kind`, `to` and an optional
  free-text `note`. The schema puts no bound on a note; the soup's door
  takes a new note of at most 1,000 characters (on every edge, of either
  schema) and reads a longer one written before that cut to 1,000, naming
  the cut in `truncated`.
- `to` is the region's id. In a record, a bare `R-3` names a record of the
  same batch first, else the region `R-3` the soup already holds; `R-3@ws`
  always names a region already held.
- A claim of a node that is not a region is refused by the grammar.
- Nothing is required at mint: a cluster may be written before it claims
  anything.

What the grammar does not judge: a claim of a retired region is not refused,
two clusters may share a handle, and a boundary's wording is not checked.

## Writing clusters and claims

- `soup_put` (MCP) and `cauldron soup import` (command line) take cluster
  records beside workspace records in ONE batch, all or nothing: a batch may
  make a region, a cluster and the cluster's claim of that region together.
  Each record goes to its pool by its `type`.
- `cauldron soup import-corpus` lands an older corpus's clusters (the seed
  corpus format, version 5) as `cluster` records in the same batch as its
  regions and nodes: the name is the handle, the description the boundary,
  and each region of the input the cluster names is a `claims` edge. What a
  cluster record cannot hold is listed under `corpus.not_carried`.
- `soup_claim {cluster, region, release?, note?}` (MCP) and
  `cauldron soup claim <cluster> <region> [--release] [--note <text>]`
  add one claim, or remove it with `release`. Both are idempotent: claiming
  what is claimed, or releasing what is not, writes nothing and says so.
- A claim touches the region it claims: while a ripple holds that region
  (`region_locked`), the claim is refused as `soup_link` is.
- The review flags (`meta.approved`, `meta.rejected`) are refused on a
  cluster as on every record.
- A ripple writes no cluster and no claim in this release.

## Reading them

- `soup_region` lists the clusters that claim a region (`clusters`).
- `soup_nodes --type cluster` (`cauldron soup nodes --type cluster`) lists
  clusters, each with the regions it claims.
- `soup_get CL-n` answers a cluster as a record, with its claims.
- `soup_search` matches cluster handles and boundaries.
- `soup_status` counts them; `soup_changes` reports cluster and claim writes
  on the same cursor as every other write: a cluster's creation as `create`,
  a claim as `link` (its id the cluster's), a release as `unlink`.

## The change feed's ops can grow

Each soup change in `soup_changes` (and `cauldron soup changes`) carries an
`op` saying what the write did. The set of ops is not closed: this release
adds `unlink`, and a later one may add another. A consumer must ignore an op
it does not know (skip the entry, or treat its `ids` as changed and read
them again) and never fail on it; the entry's `seq` and the answer's
`cursor` move past it as past any other. Releases 0.1.6 to 0.1.15 listed the
ops as exactly `create`, `link` and `update` and said nothing about an op
outside that list; a 0.1.14 or 0.1.15 service reports a released claim as
`update` with no ids.

## A cauldron made before this grammar

A cauldron made by 0.1.4 to 0.1.15 has no `cauldron` pool. It gets one on its
first cluster write (its first batch that lands a cluster record; a claim
never adds it), as one additive act:
the pool file is built aside and moved into place whole, nothing else in the
store is changed, and nothing is migrated. A store with the pool still opens,
reads and writes under 0.1.14 and 0.1.15, whose ordinary reads do not show
clusters (its command line reads and writes the pool raw only when asked for it by
name, `--pool cauldron`, and its pinned grammar judges such a write).
