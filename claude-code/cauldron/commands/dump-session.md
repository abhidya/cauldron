---
description: Keep this Claude Code session's transcript in a Cauldron cauldron, verbatim, as a dump
argument-hint: [title]
---

Keep this session's transcript in a Cauldron cauldron as a **dump**, for
later processing. It lands as `DM-<n>` in state `received` in the
cauldron's dumps store, beside its soup, and waits there until it is
processed: by its owner (`/cauldron:curate-dump`, or `cauldron ripples run
<DM-n>` at a terminal), or on its own when the owner turned automatic
processing on for that cauldron with `--include-sessions` (the receipt's
`automatic` says whether it takes this dump; it is off unless the owner
turned it on).

Title: $ARGUMENTS

Call the cauldron MCP tool `soup_dump_session` (this plugin's server) with
`session_id` set to `${CLAUDE_SESSION_ID}`, and `title` set to the title
above when one was given. Add `cauldron` only if the user named one. Do not
read the transcript yourself and do not pass a path: the plugin's bridge
finds the transcript on this machine by the session id.

The transcript is stored **verbatim**: every message, every tool call and
its output, and anything pasted into the session, including any secret that
passed through it. It stays in the user's own cauldron on this machine. If
the user has not said they want that, tell them so in one sentence before
calling.

Refusals: `not_granted` or `no_cauldron_for_project` (show the refusal and
the `cauldron mcp grant …` command it names; the user runs it at a terminal,
and you run it only if the user asks and the owner opened `grant` to agents), `service_down` (show the start command it names),
`session_unknown` (no transcript of that id on this machine; say so),
`transcript_unreadable` (show the reason), `transcript_link` (a link was
found where the transcript is looked for, and nothing was read; show the
refusal, and do not try to work around it), `too_large` (over 64 MiB; say
so). There is no CLI route for this one.

A dump cannot be deleted in this release; the plugin's README says how to
remove a cauldron's dumps by hand.

Report the receipt: the dump's id, the session id, its lines and bytes, its
sha256, and what `automatic` says (processed on its own, or waiting for its
owner). Never repeat the transcript back.
