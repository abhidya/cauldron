---
description: Turn a dump kept in a Cauldron cauldron into soup records that cite it, through the curator subagent
argument-hint: <DM-n> [cauldron]
---

Dispatch the `cauldron:curator` subagent to read one **dump** (`DM-<n>`, kept
with `/cauldron:dump` or `/cauldron:dump-session`) and turn what in it is
worth keeping into soup records, each citing the dump.

Arguments: $ARGUMENTS

The first argument is the dump's id (`DM-<n>`); without one, call
`soup_dumps` (with `state: "received"`) and ask the user which dump to curate.
The second, if given, is the cauldron's name.

**Find the target.** This command works through the cauldron MCP tools (this
plugin's server; `soup_status` among them). Call `soup_status`, with
`cauldron` set when the user named one. It answers the environment the MCP
door serves, the cauldron, `resolved_from` and the grant's `scopes`.

- Reading a dump's content needs a grant of both `read` and `dump`, and
  writing records needs `write`. If `scopes` lacks one, or the answer is
  `not_granted` or `no_cauldron_for_project`, show the user the refusal and
  the `cauldron mcp grant …` command it names (they run it at a terminal; you
  cannot), and stop.
- `service_down`: the `mcp` service is not running. Show the start command
  the refusal names and stop. (Without the service, the curator can still
  read the dump with `cauldron soup dump-get <id> --view text` and land
  records with `cauldron soup import`; offer that only if the user asks.)
- Call `soup_dumps` and check that the dump is there; say its kind, title,
  size and state to the user.

**Confirm before writing to `prod`.** When the target did not come from the
user's own words (it came from `soup_status`'s defaults or a grant's
project), tell the user the environment, the cauldron and `resolved_from`
before dispatching. If that environment is `prod`, ask the user to confirm
writing there first, and dispatch only once they have.

Hand the curator, in its prompt: the cauldron name and the environment (as
resolved, and from where, and whether the user confirmed a `prod` target),
that it is to use the MCP tools, the dump's id, and these instructions:

1. Read the dump in pages with `soup_dump_get`: for a session dump
   (`kind: session`) with `view: "text"`, which answers only what the
   person typed and what the assistant wrote as text (tool calls and their
   output, thinking, `!` shell commands and their output, local commands'
   output, slash-command wrappers and system reminders are left out and
   counted); for a text dump, without `view`. Read on with `offset` set to
   the page's `next_offset` (and `turn_offset` to its `next_turn_offset`
   when there is one) until `more` is false. Use `max_chars` of at most
   20000 per page.
2. Decide what in the dump is worth a node, exactly as for any other
   material: decisions, rules learned, defects, open questions, ideas.
3. Give every record `meta.cites` containing `{"kind": "dump", "id": "<DM-n>"}`
   (and, for a turn it rests on, `"at": <the turn's at>` in that citation).
4. Judge the batch with `soup_put` (`remint: true`, `dry_run: true`), fix
   until every record is accepted, then land it, and report the new ids.

The dump itself is not changed: it stays `received`, and the records point
at it through their citations. Never repeat a transcript's text back beyond
what a record needs, and never copy a secret from a dump into a record.

When the curator returns, relay its report: the new ids with their handles
and regions, anything it left out, and any region it proposes. A proposed
region is never created without the user's approval; if they approve it,
dispatch the curator again with the approved region and the records it could
not place.
