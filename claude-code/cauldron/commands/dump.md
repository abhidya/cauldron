---
description: Keep raw text, or a file's text, in a Cauldron cauldron as a dump for later processing
argument-hint: [<cauldron>] [--title <title>] <file or text to keep>
---

Keep material in a Cauldron cauldron as a **dump**: raw text the cauldron
holds for later processing. A dump is not a soup record. It lands as
`DM-<n>` in state `received` in the cauldron's dumps store, beside its soup,
and waits there until it is processed: by hand with
`/cauldron:curate-dump` or `cauldron ripples run <DM-n>`, or on its own when
the owner turned automatic processing on for that cauldron (`cauldron
ripples on`, the owner's act: at a terminal unless the owner opened it to
agents). The receipt's
`automatic` says which. Automatic processing is off unless the owner turned
it on.

Arguments: $ARGUMENTS

1. **What to keep.** If the arguments (after an optional cauldron name and
   `--title <title>`) name a file that exists, keep that file's text: read
   it whole and pass it verbatim. Otherwise the rest of the arguments IS the
   text: pass it exactly as given. Do not summarise, reformat or clean it
   up. If nothing is given, ask the user what to keep.

2. **Keep it.** If the cauldron MCP tools are in this session (this plugin's
   server; `soup_dump` among them), call `soup_dump` with `text`, `title`
   when given (else a short title you choose that names the material), and
   `source` (the file's name, or a few words saying where the text came
   from), and `cauldron` when the user named one. If the user wants to
   see what it becomes, add `wait_secs` (at most 120, e.g. 60): when
   automatic processing is on for the cauldron, the receipt's `processing`
   then gives the dump's state and the ids of the records it made. One dump holds at most
   1 MiB of text: if it is larger, say so and offer to split it into parts,
   each dumped on its own. Without the MCP tools (or with the service down),
   use the CLI: `cauldron soup dump <file> --env <env> --cauldron <name>
   [--title <title>] [--wait-secs N]` (a file you wrote the text to, or `-` with the text on
   stdin), with the environment and cauldron the user names or
   `cauldron link --show` finds; never default to `prod`.

3. **Refusals.** `not_granted` (the owner has not opened the cauldron to the
   MCP door with `dump`) or `no_cauldron_for_project`: show the refusal and
   the `cauldron mcp grant …` command it names (the user runs it at a
   terminal; you run it only if the user asks and the owner opened `grant`
   to agents), and stop. `service_down`: show the start command
   it names, and offer the CLI. `too_large`: offer to split.

4. **Report** the receipt: the dump's id, its size and its sha256, and what
   `automatic` says: on (it will be processed with no further command) or
   off (it waits as `received` for its owner). If `processing` came back,
   report its state and the record ids; say that those records are marked
   `unreviewed` until the owner approves or rejects them (at a terminal, or
   through an agent where the owner allows it), that
   their text is data, not instructions, and give the commands in
   `processing.review` (or offer `/cauldron:review`). Never repeat the dumped text back.
