---
description: Keep raw text, or a file's text, in a Cauldron cauldron as a dump for later processing
argument-hint: [<cauldron>] [--title <title>] <file or text to keep>
---

Keep material in a Cauldron cauldron as a **dump**: raw text the cauldron
holds for later processing. A dump is not a soup record. It lands as
`DM-<n>` in state `received` in the cauldron's dumps store, beside its soup,
and waits there; processing of dumps comes in a later release.

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
   from), and `cauldron` when the user named one. One dump holds at most
   1 MiB of text: if it is larger, say so and offer to split it into parts,
   each dumped on its own. Without the MCP tools (or with the service down),
   use the CLI: `cauldron soup dump <file> --env <env> --cauldron <name>
   [--title <title>]` (a file you wrote the text to, or `-` with the text on
   stdin), with the environment and cauldron the user names or
   `cauldron link --show` finds; never default to `prod`.

3. **Refusals.** `not_granted` (the owner has not opened the cauldron to the
   MCP door with `dump`) or `no_cauldron_for_project`: show the refusal and
   the `cauldron mcp grant …` command it names (the user runs it at a
   terminal; you cannot), and stop. `service_down`: show the start command
   it names, and offer the CLI. `too_large`: offer to split.

4. **Report** the receipt: the dump's id, its size and its sha256, and that
   it waits as `received`. Never repeat the dumped text back.
