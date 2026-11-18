# Cauldron for Claude Code

Use a Cauldron **soup** (a cauldron's typed knowledge graph) from a Claude
Code session: the plugin's MCP server gives the session tools to read,
search and write the soups their owner has opened to it, and to keep raw
text or a whole session transcript in a cauldron as a **dump** for later
processing; a `curator`
subagent turns notes, a conversation excerpt, findings or a dump into
schema-valid soup records and lands them, and the published schema ships with the plugin
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
| `commands/dump.md` | `/cauldron:dump [<cauldron>] [--title <t>] <file or text>`: keep text as a dump |
| `commands/dump-session.md` | `/cauldron:dump-session [title]`: keep this session's transcript as a dump |
| `commands/curate-dump.md` | `/cauldron:curate-dump <DM-n> [cauldron]`: turn a dump into soup records that cite it |

The dump ripple (`cauldron ripples run`, and `cauldron ripples on` for
automatic processing) is a terminal command, not part of this plugin; its
records reach a session as `unreviewed` (see [Ripples](#ripples)).
| `regions.example.json` | four example regions to import into a new, empty soup |
| `skills/soup-schema/SKILL.md` | `/cauldron:soup-schema`: the schema in brief, with a pointer to the full README |
| `schema/` | a copy of the published schema (`schema/soup/workspace-v6/` in the Cauldron repository): `README.md`, `grammar.json`, `edges.json`, one JSON Schema per node type, and its `--remint` variant (batch labels for ids) |

`schema/` is generated: `scripts/sync-plugin-schema` copies it from the
published directory, and a test in the spine crate fails when the copy drifts.
Do not edit it here.

## Install

Cauldron's consumers use the environment `prod`, which `scripts/install`
makes; every command here names it. The path from a fresh machine:

1. Install Cauldron from a checkout (`./scripts/install`; the repository's
   README has the details). It makes `prod` and runs its `mcp` service. An
   environment installed before that service existed gets it from
   `./scripts/install --replace-running`, and one whose `mcp` service is an
   older build has it rebuilt and re-pinned by the same command (without
   the flag, the install says the service is still the old build and
   prints the two commands that upgrade it).
2. Put `<root>/bin` on your `PATH` (`<root>` is `~/.cauldron`, or
   `$CAULDRON_HOME`). The plugin starts its server as the bare command
   `cauldron mcp stdio`, so `cauldron` must be on the `PATH` Claude Code
   itself sees: start Claude Code from a terminal where `command -v
   cauldron` finds it. Claude Code started from the Dock, Finder or an IDE
   may not read your shell startup file, and its sessions then have no
   cauldron tools. If your cauldrons live under `$CAULDRON_HOME`, set it
   there too.
3. Make a cauldron, with an empty soup (one pool, `ws`):

   ```sh
   cauldron create kb --env prod
   ```

4. Give its soup regions. A new soup has **no regions**, and the curator
   never invents one: it files nothing it cannot place and proposes regions
   instead. This plugin ships `regions.example.json`, four regions (the
   code, its tests, the product, the process) whose ids are batch labels.
   Copy it, edit the handles and boundaries to fit the project, and import
   it:

   ```sh
   cauldron soup import regions.json --env prod --cauldron kb --remint --dry-run
   cauldron soup import regions.json --env prod --cauldron kb --remint
   ```

5. Open the cauldron to Claude Code, at a terminal:

   ```sh
   cauldron mcp grant kb --env prod --project ~/code/proj
   ```

6. Install the plugin, inside Claude Code:

   ```text
   /plugin marketplace add broomstick-ai/cauldron
   /plugin install cauldron@cauldron
   ```

   (From a checkout of the repository instead: `/plugin marketplace add
   <checkout>`, then the same install.) Restart Claude Code; in a session,
   `/mcp` lists `cauldron` as connected.
7. In a session in `~/code/proj`, `/cauldron:curate <what to record>`.

> **Two ways to say "this directory means cauldron `kb`".** They are
> separate, and setting one does not set the other.
>
> | | `.cauldron` marker | grant `--project` |
> |---|---|---|
> | made by | `cauldron link kb --env prod` | `cauldron mcp grant kb --env prod --project <dir>` |
> | read by | the `cauldron soup` commands (and the curator's shell fallback) | Claude Code sessions, through the MCP door |
> | opens the cauldron to Claude Code? | no, never | the grant does; `--project` only sets the directory's default |
> | kept in | a file in the project (keep it out of git) | `<root>/data/prod/mcp/grants.json` |

> **`$ENV`.** `cauldron create`, `cauldron mcp grant` and the other
> commands take their environment from `--env`, else `$CAULDRON_ENV`, else
> `$ENV`, else `prod` (the `cauldron soup` commands never fall back to
> `prod`); the plugin's bridge takes
> `$CAULDRON_ENV`, else `prod`, and never reads `$ENV`. In a shell that
> exports `ENV` (`ENV=development`, or a startup file's path), `cauldron
> create kb` without `--env` aims at that environment while sessions use
> `prod`. Name `--env prod`, or unset `ENV`.

**A developer's note.** To try the plugin from a checkout for one session,
without installing it: `claude --plugin-dir <checkout>/claude-code/cauldron`.
To work against another environment than `prod` (`cauldron env create dev`),
give it an `mcp` service yourself (`cauldron --env dev build mcp --env-copy`,
then `cauldron --env dev pin mcp <the label that build printed>`), and start
Claude Code with `CAULDRON_ENV=dev` so the bridge serves it.

### A project's `.cauldron` marker

To make a project directory name its cauldron for the `cauldron soup`
commands, so neither you nor the curator's shell fallback has to pass
`--env` and `--cauldron` inside it:

```sh
cd <project>
cauldron link kb --env prod        # writes <project>/.cauldron
cauldron link --show               # what this directory resolves to, and from where
```

`cauldron soup` run in that directory or any directory below it then uses
`kb` in `prod`. The marker is a text file holding two `key = value` lines,
`env = prod` and `cauldron = kb`, and `#` comments. An explicit `--env`,
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

### Renaming or removing a cauldron

`cauldron rename kb notes --env prod` renames a cauldron; its soup, its dumps
and its grant follow (the grant keeps its scopes and projects).
`cauldron remove kb --env prod` says what it would do; with `--yes` it moves
the cauldron to `cauldrons/.removed/<name>-<stamp>/` (nothing is deleted) and
drops its grant. Both are refused while the `mcp` service holds the cauldron:
stop the service (`cauldron --env prod stop mcp`, and `start mcp` after) or
revoke the grant (then grant it again). A `.cauldron` marker naming the old
name is not updated: run `cauldron link notes --env prod --replace` in that
project.

## The MCP door

The plugin's `.mcp.json` starts `cauldron mcp stdio` with each session. That
bridge talks to the environment's `mcp` service, a supervised service that
listens on `127.0.0.1` only and is the one writer of every soup it holds.

**Every cauldron is closed to it until you grant it.** Granting is your act,
at a terminal on this machine:

```sh
cauldron mcp grant kb --env prod --project ~/code/proj # run it yourself, in a terminal
cauldron mcp grants --env prod                         # what is open
cauldron mcp revoke kb --env prod                      # close it again
cauldron mcp status --env prod                         # the service, its version, what it holds
cauldron --env prod start mcp                          # if the service is not running
```

- `grant` runs only with its input and output on an interactive terminal, and
  no option skips that: an agent, whose shell has no terminal, cannot open a
  cauldron for itself, and no MCP tool can grant, revoke or list grants. The
  default scopes are `read,write,dump` (`--scopes read` opens it read-only;
  `dump` lets sessions keep dumps in it, see below).
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
- `status` gives the running service's `version` beside this `cauldron`'s.
  When they differ (an upgrade left the environment's `mcp` pin on its old
  build), it says so under `version_skew` and gives the two commands that
  upgrade it under `upgrade`. A session says the same: the service's
  instructions carry the note, and a tool or an argument that the older
  service does not know is refused with it, naming both versions.
- `revoke` takes effect on the next call: the service closes the cauldron and
  lets go of its soup. Writes still waiting in its queue are refused
  (`not_granted`) and never land; a write already being applied when the
  revoke comes finishes, all of it or none of it. `revoke --project <dir>`
  removes only that binding.
- A grant is for the cauldron it was made for, not only its name: it is
  bound to the random id in the cauldron's `cauldron.toml`, which a rename
  and a move of the whole data root keep. If that cauldron is removed and
  another is made under the same name (even with the same creation time in
  its `cauldron.toml`), calls are refused as `cauldron_gone` until you grant
  the new one. A cauldron made before ids is given its id by its first
  writer (the `mcp` service, before a call reaches it), and its grant is
  carried onto the id then if it records the cauldron's creation time; one
  that does not is refused as `not_granted` with the command that grants it
  again. A grant written by
  a pre-release build, which names its cauldron only by name, is refused as
  `not_granted` with the `cauldron mcp grant` command that makes it again.
  It is named before that first refusal too: `cauldron mcp status`
  (`grant_note`, `regrant`), `cauldron mcp grants` (`warnings`) and the
  closing lines of `scripts/install` on an upgrade each say so, with that
  command.
- The grants are kept in `<root>/data/<env>/mcp/grants.json`, owner-only.

The tools: `soup_status` (which cauldron this session reaches and why, its
counts and change cursor), `soup_regions`, `soup_region`, `soup_nodes`,
`soup_search` and `soup_get` (the same answers as the `cauldron soup` reads,
each reply's payload bounded by `max_chars`: default 12,000, floor 256,
ceiling 50,000; a `soup_dump_get` page is never longer than its
`max_chars`, which is at least 1,000 and refused below it), `soup_put` (records in the published shape,
judged and landed as one all-or-nothing write, with `dry_run`, `remint`,
`partial`, `cite` and `allow_duplicate`; the session is cited on every record
when Claude Code passes its id), `soup_link` (edges between nodes the soup
holds) and `soup_changes` (what landed since a cursor, the soup's changes
and the dumps), and the dump tools below. `soup_claim` is listed and always
refuses (`unsupported`: this schema has no clusters). A refusal is a tool
result with a
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

Every write through the door, and every `soup_dump_get` (which dump, the
offset and how many bytes were read), adds a line to
`<root>/data/<env>/mcp/calls.jsonl` (when, which client and session, which
tool, the outcome and counts; never record text, and never a dump's text or
transcript). Once it reaches 8 MiB it is moved to `calls.jsonl.1`, replacing
an older one.

## Dumps

A **dump** is raw text put into a cauldron for later processing: a note, a
paste, a file, or a whole Claude Code session. It is not a soup record and
does not go into the soup. It lands as `DM-<n>` in state `received` in the
cauldron's **dumps store**, `<cauldron>/dumps/` beside its soup (its own
small spine and a content-addressed blob store, made on the first dump), as
one write. Nothing processes a dump on its own unless the owner turned
automatic processing on for its cauldron ([Automatic
processing](#automatic-processing)): it waits as `received` until you curate
it (below) or run the dump ripple on it by hand (`cauldron ripples run`, see
[Ripples](#ripples)). A soup record
that came from a dump can cite it: `meta.cites: [{"kind": "dump", "id": "DM-3"}]`.

- `/cauldron:dump <file or text>` (the tool `soup_dump`) keeps up to 1 MiB of
  text, verbatim; larger is refused as `too_large`, to be split. It takes a
  `title`, a `source` (where the text came from), a `cite` and `refs` (soup
  ids it is about).
- `/cauldron:dump-session [title]` (the tool `soup_dump_session`) keeps this
  session's transcript, or another session's by `session_id`. The bridge,
  which runs as you beside the session, finds the transcript by the session's
  id, only as `<config>/projects/*/<id>.jsonl` under `$CLAUDE_CONFIG_DIR`
  (else `~/.claude`), and streams it to the service; no tool takes a path,
  and the service reads none. It follows no link there: a project directory
  or a transcript that is a link, a transcript with a second name (a hard
  link), or one whose real path is outside `<config>/projects` is refused as
  `transcript_link`, and a FIFO, device or directory in its place as
  `transcript_unreadable`; nothing of it is read. At most 64 MiB.
  `session_unknown` when there is no transcript of that id;
  `transcript_unreadable` when the store or the file cannot be read. The
  stored blob's sha256 is the transcript file's.
- **A session transcript is stored verbatim.** It holds every message, every
  tool call and its output (file contents, command output), and anything
  pasted into the session, including any secret that passed through it. It
  is kept as it is, in your own cauldron on this machine, in the cauldron's
  dumps store.
- **The dumps store is yours alone.** `dumps/` and `dumps/blobs/` are mode
  0700 and every file in them (the databases, which hold a text dump's
  text, and the blobs) 0600, whatever your umask. A store made by a build
  that did not do this is set so the next time it is opened for writing.
- Every dump answers a receipt: its id, `state: received`, size, sha256 and
  the change cursor; never the text.
- `soup_dumps` lists the dumps (`read`; never their text), and
  `soup_dump_get` reads one's content from a byte `offset`, as much as fits
  `max_chars`, with `next_offset` while there is more. For a session
  transcript, `view: "text"` answers only what the person typed and what
  the assistant wrote as text, as turns each with `at` (the byte where its
  line starts), `role` and `text`. Everything else is left out and counted
  in `elided` by kind: tool calls (`tool_calls`) and their results
  (`tool_results`), thinking, `!` shell commands and their output (`shell`),
  local commands' output (`local_command`), slash-command wrappers
  (`commands`), system reminders (`system_reminders`; typed words around
  one are kept), other text Claude Code writes itself such as a compaction
  summary (`generated`), `isMeta` lines (`meta`), attachments, images and
  other blocks (`other_blocks`), other lines (`other_lines`) and lines that
  are not JSON (`unreadable_lines`). The raw view, like the stored blob,
  still holds the whole transcript, all of that included. Its pages end between turns and read the same every time; a turn
  longer than a page is cut (with `chars`, its whole length), and
  `next_turn_offset` (passed back as `turn_offset`) goes on in it. Reading a dump's
  content needs both `read` and `dump`: a dump is raw material, and a
  transcript can hold tool output and pasted secrets, so a grant made only
  to read the knowledge graph does not hand it out.
- Writing a dump needs the `dump` scope. The service opens a cauldron's
  dumps store on its first dump and holds it until the grant loses `dump`
  or is revoked, or the service stops. It reads the grant again just before
  each dump is written, and every 4 MiB while a transcript arrives.
- **What the grant covers.** The scopes govern Claude Code sessions, which
  reach dumps through the MCP service. `cauldron soup dumps` and `cauldron
  soup dump-get` (`--raw` included) run as you and read any dump without a
  grant, like every other CLI read; so a session allowed to run shell
  commands as you can read dumps that way too. The scopes gate the MCP door;
  they do not lock the files.
- **A dump that does not land leaves nothing.** A dump refused or failed at
  any step (a grant revoked while a transcript arrives included) leaves no
  bytes: its partly written file is removed, and so is any blob no dump
  names. If a writer is killed while a dump is written, what it left is
  removed the next time that dumps store is opened for writing.
- **Removing a dump.** If a dump holds something it must not (a secret in a
  transcript), remove it from a shell; there is no MCP tool for it, by
  design:
  1. Revoke the cauldron (`cauldron mcp revoke <cauldron> --env <env>`) or
     stop the service (`cauldron --env <env> stop mcp`): while the service
     holds the dumps store, a removal is refused and is not sent to it,
     because it needs the store's databases to itself.
  2. `cauldron soup dump-remove <DM-n> --env <env> --cauldron <cauldron>`
     is a dry run: it removes nothing and says what it would do (exit 2).
     Add `--yes` to do it. `--env` and `--cauldron` must be flags.
  3. Grant the cauldron again (or start the service).

  The dump stays as a tombstone, so a record that cites it still finds it:
  state `removed`, with its id, kind, size and when it was received and
  removed. Everything else goes: its blob is deleted, its title, source,
  cites, refs and sha256 are dropped, and its text is erased from the dumps
  store's record of its writes, whose databases are then compacted, so
  nothing of it is left in the cauldron's directory. If another dump holds
  the same bytes, the blob is kept and that dump is named
  (`same_bytes_in`): remove it too. Copies made elsewhere (a backup, a
  cauldron put away with `cauldron remove`) are not touched, and neither are
  the records a ripple drew from the dump (find them by their cite,
  `{"kind": "dump", "id": "DM-<n>"}` in `meta.cites`, and supersede any you
  do not want) nor the threads service's copy of each ripple call's prompt
  and answer. Treat a secret that was dumped as exposed, and change it.

  A removal runs in two phases. It first marks the dump `removing`, and from
  then on nothing reads its content. It then erases, and only once the
  erasure has finished does it record `removed`. A removal cut short (a
  kill, a crash, a reader holding the database) leaves the dump `removing`:
  `soup dumps` and `soup dump-get` say `erasure: unfinished` and print the
  command that finishes it (`finish_with`). Running the removal again
  finishes it, and so does the next open of the dumps store for writing (a
  `soup dump` from a shell, or the service's first dump once it holds the
  store).
- From a shell: `cauldron soup dump <file|-> --env E --cauldron C [--title T]
  [--source S]`, `cauldron soup dumps --env E --cauldron C` and `cauldron
  soup dump-get <id> --env E --cauldron C [--offset N] [--max-chars N]
  [--raw] [--view raw|text] [--turn-offset N]` (`--raw` prints the content
  verbatim; `--view text` is the text view above). While the service holds
  the dumps store, `soup dump` is sent to it (its JSON says `forwarded_to`);
  otherwise it writes directly.

**Curating a dump.** `/cauldron:curate-dump DM-3 [cauldron]` turns a dump
into soup records. It checks the target and the grant's scopes (reading the
dump needs `read` and `dump`, landing records needs `write`), asks you to
confirm before writing to `prod`, and hands the dump's id to the curator,
which has `soup_dumps` and `soup_dump_get` among its tools. The curator reads
the dump in pages (a transcript through the text view), decides what is
worth a node, judges the batch with a dry run, lands it, and reports the new
ids. Every record it writes cites the dump: `meta.cites: [{"kind": "dump",
"id": "DM-3"}]`, with the turn's `at` when a record rests on one turn. The
dump itself is not changed: it stays `received`.

## Ripples

A **ripple** turns one dump into new soup records (ideas, issues, questions,
decisions, constraints, tasks and dangers), each citing the dump. It runs
when you run it, at a terminal, on the one dump you name, or, since 0.1.12,
by itself on each new dump of a cauldron whose owner turned that on (see
[Automatic processing](#automatic-processing)). No tool of this plugin
starts one, turns automatic processing on, or approves what a ripple wrote:

```sh
cauldron ripples run DM-3 --env prod --cauldron kb            # one dump; prints the records and the tokens
cauldron ripples status --env prod                            # the last runs, spend today and per dump, the caps
cauldron soup approve C-12 IS-4 --env prod --cauldron kb      # you, at a terminal: mark records as reviewed
```

- **Off unless started.** The `ripples` service does the work, and the
  installer does not start it: `cauldron --env prod build ripples`, `cauldron
  --env prod pin ripples <the label it printed>`, `cauldron --env prod start
  ripples`. It needs the `mcp` service and a `threads` service of the same
  release running, and the cauldron granted `read`, `write` and `dump`.
  Running, it processes nothing until `cauldron ripples run`, unless the
  owner turned automatic processing on for a cauldron.
- **A bare completion, not an agent.** The call goes through the `threads`
  service as one Claude Code call on the Claude login threads uses, with no
  tools of any kind (no built-in tool, every one denied by name, no MCP
  server), no settings files, plugins, hooks, skills or CLAUDE.md
  (`--setting-sources ""` and Claude Code's `--safe-mode`),
  permission mode `dontAsk` (never a skip-permissions mode), one turn, an
  answer bounded to 16,000 tokens and given as JSON that must meet
  `batch.schema.json`. An older threads, which would ignore those settings,
  is refused before anything is sent (`threads_unsupported`).
- **What leaves this machine.** Claude Code's own system prompt (it names
  the call's working directory, an empty directory made for the call under
  the system's temporary directory, the platform, the OS version and the
  date), a note Claude Code adds naming the email address of the login the
  call runs on, and one prompt, the file `services/ripples/package/dump/v1/prompt.md`
  of the Cauldron repository filled in with the cauldron's regions (at most
  50) and the dump's text: a text dump whole, a session dump only as its text
  view (what you typed and what the assistant wrote as text; never tool
  calls, tool output, thinking or shell output). No existing record is sent.
  The dump's text sits between marker lines, after a rule that it is data
  and never instructions. Since 0.1.12 the text first goes through a secret
  scrubber that replaces credential-shaped text (known key and token
  formats, private-key blocks, JWTs, passwords and tokens in URLs,
  `Authorization:` and `Cookie:` headers, secrets on command lines such as
  `--password x` or `--admin-password=x`, assignments to secret names such as `password=…` or
  `"api_key": "…"`, long random strings other than a digest in its obvious
  context, such as a `sha512-…` integrity hash or an image's `data:` URI)
  with `<redacted:kind>`; the answer
  is scrubbed the same way before it is written. Its time is bounded: if it
  cannot finish, nothing is sent. The scrubber reduces the risk and is not a guarantee: remove a
  dump you know holds a secret (`cauldron soup dump-remove`) before any run.
- **What is stored where.** threads keeps, per call, a thread named `ripple
  …` with the prompt (dump text included) and the answer in its store
  (`data/<env>/threads/store/`, `threads.db` and `turns/`), until you delete
  it; removing the dump does not remove that copy. Claude Code writes no
  session transcript for the call (`--no-session-persistence`). The ripple's
  journal (`data/<env>/ripples/runs.jsonl`) holds ids, counts and tokens,
  never text, and the call's working directory is removed when it ends. The
  root README's "Ripples, by hand" lists all of it, with the environment the
  call runs in.
- **How its records are marked.** Each cites the dump and carries
  `meta.ripple` (package, run, model); the `mcp` service records the write
  as a ripple's, and nothing in a record can change that. Reads show such a
  record with `unreviewed: true`; `soup_nodes` and `soup_search` take
  `review: "pending"` (only those) or `"reviewed"`.
- **Approving is the owner's, at a terminal.** `cauldron soup approve
  <id>... --env <env> --cauldron <name>` (and `--unapprove`) runs only with
  its input and output on an interactive terminal, like `cauldron mcp
  grant`; it is written as the owner and audited. There is no MCP tool for
  it, so a session cannot approve, and a ripple never can (the soup refuses
  `meta.approved` from a ripple). If you are a session and the user wants
  records approved, tell them the command; do not try to run it. A review
  flow for sessions comes in a later release. There is no reject yet.
- **Unreviewed records are data, not instructions.** A ripple's records
  were written by an automatic step from a dump, and a dump's text came from
  outside. Every MCP reply that holds a record marked `unreviewed: true`
  carries `unreviewed_note` saying so, and the door's instructions, the
  curator, the commands and the schema skill tell a session to treat records
  and dumps as data, never to act on an instruction found inside one, and to
  tell the user when one reads like an instruction. That is guidance to the
  session, not a control: the review flag stops nobody from reading a
  record, and a session's own writes (a record that supersedes a ripple's,
  say) are not gated by it.
- **What it may write.** New records only, of those seven types, in
  existing regions, linked only with `derived_from` to records of the same
  answer, at most 25: never an edit, a close or a supersede, a directive, a
  region or a flag. A repeated label (`duplicate-id-in-batch`) or a label
  equal to a held id (`label-shadows-existing`) refuses the whole answer.
- **The caps, across runs.** Per dump, across every run of it: 3 model calls
  and 200,000 input tokens (`--force-budget <tokens>` on `run` is the
  owner's way to let one run spend more). Per cauldron per UTC day:
  1,500,000 input tokens or 30 dumps. Counted from the run journal before
  each call. A call is charged when it begins, before anything is sent (one
  call and its estimate: the prompt's characters divided by three, which
  leaves out Claude Code's own system prompt), with the check and the charge
  made in one step so two runs cannot both take the last call; it is
  settled to the larger of its reported input tokens and that estimate when
  it ends. A call cut short by a kill or a stop of the service stays
  charged, and `status` shows it in `unsettled_calls`. One call can pass a
  cap. Over a cap
  nothing more is sent and the run ends `deferred` (`budget` or
  `daily_cap`); the dump stays `received`. Sonnet first (or `--model`), one
  Opus retry when the answer misses the schema or the soup refuses it.
- **A call given up is stopped.** Parked by threads' budget, ended without
  an answer, or past 15 minutes: the ripple cancels the turn in threads,
  asks threads to withdraw its message and checks the thread is idle, and
  the outcome says whether that was confirmed or how to stop it. A call
  left by a killed or stopped service is cancelled when the service next
  starts or runs.
- **The dump.** Processed, it moves from `received` to `processed`, naming
  the run and the records (`soup_dumps`, `soup_dump_get`). Curating by hand
  leaves it `received`. A removed dump, or one whose removal is unfinished,
  is refused. A dump run twice gets one set of records.

### Automatic processing

New in 0.1.12, and **off by default for every cauldron**; an upgrade never
turns it on. The owner turns it on for one cauldron, at a terminal:

```sh
cauldron ripples on kb --env prod [--backend threads|local] [--model <m>] [--include-sessions] [--no-scrub-local]
cauldron ripples off kb --env prod        # from anywhere; before the next call
cauldron ripples status --env prod        # on/off, since when, backend, queue, spend today
```

- **What it means.** Each dump that lands in that cauldron after that
  moment is processed with no further command, exactly as `cauldron ripples
  run` would: the same bare call, caps, journal and `unreviewed` marking.
  Older dumps are processed only by hand. Text dumps only, unless
  `--include-sessions`. `on` authorises spend and writes nobody asks for one
  by one, so it requires an interactive terminal (like `cauldron mcp
  grant`): it is the owner's act, a session must not try to turn it on, and
  no tool of this plugin does. `on` prints what is sent where and the caps,
  then asks the owner `[y/N]`.
- **One cauldron, never a name.** The setting is bound to the cauldron it
  was turned on for, by the cauldron's id (as its grant is). Removed,
  renamed, or made again under the same name, it is not carried over:
  `cauldron remove` and `cauldron rename` turn it off, and a setting left
  behind or copied is void (nothing runs; `status` says why and how to
  clear it). A data root moved whole keeps it. The terminal check guards
  the command, not the file: anyone who can write the data root can write
  the setting, as they can `grants.json`.
- **What is sent where.** `--backend threads` (the default): the scrubbed
  text and the regions to Anthropic, through threads on this machine's
  Claude login, as above. `--backend local`: to the `inference` service on
  this machine; nothing leaves it; scrubbed too unless `--no-scrub-local`.
- **The service.** The `ripples` service must be running (the installer
  does not start it). The `mcp` service tells it when a dump lands, and it
  also looks for waiting dumps at start and every minute. One dump at a
  time, the cauldron served least recently first. A dump that lands while
  another run holds the service's one run slot waits for it (`status`:
  `waits_for`), and starts when it ends.
- **Caps.** As by hand: per dump 3 calls and 200,000 input tokens; per
  cauldron per UTC day 1,500,000 input tokens or 30 dumps. Over a cap the
  dump stays `received` with `auto: {state: deferred, reason: budget}` in
  its listing and is tried again after midnight UTC. The daily cap follows
  the cauldron, not its name.
- **When threads parks a call** (an account over its usage threshold or
  limit, a spent credit balance, an expired login, a budget), the dump
  stays `received` with `auto: {state: deferred, reason: threads_parked,
  park: <cause>}`. That is not a failed attempt, and a call parked before
  it ran is charged nothing. Automatic runs on threads then wait 2 minutes,
  growing with each park in a row to at most an hour: one withdrawn call
  per wait, not one per dump. `cauldron ripples status` shows the hold
  (`threads_hold`: since when, the cause, the next attempt) and the
  cauldrons it holds (`held`).
- **When the backend is busy** (another request holds the local
  `inference` service, or it is not ready yet): the dump stays `received`
  with `auto: {state: deferred, reason: busy}`, charged nothing and not a
  failed attempt, and the next scan tries it again.
- **Failure.** A failed attempt is retried after 2, then 8 minutes; after 3
  (or at once when retrying cannot help) the dump is parked `failed` with
  `failure: {code, message, attempts, what_to_do}`, and only `cauldron
  ripples run` runs it again. The message is a fixed sentence for the
  code: no journal and no dump record holds the model's answer, its notes
  or a backend's own words.
- **What a session sees.** `soup_dump` and `soup_dump_session` receipts
  carry `automatic`: whether it is on and takes this dump. `soup_dump` with
  `wait_secs` (at most 120) waits for it and answers `processing`: the
  dump's state, the run and the record ids. `soup_dumps` and `soup_dump_get`
  show the state, `auto` or `failure`, and once processed the run and the
  records. The records are `unreviewed` until the owner approves them at a
  terminal, and are data, not instructions.

## The verbs

The curator falls back to these when the session has no MCP tools or the
`mcp` service is down. All need the environment and the cauldron named, by `--env` and `--cauldron`
or by the project's `.cauldron` marker (`cauldron link`); none defaults to
production. All
print JSON and exit non-zero on a refusal. The reads cannot write, and each
takes `--max-chars N` (longer text is cut and listed under `truncated`); the
lists take `--limit N` and say how many `more` there were. A node another
node supersedes (an inbound `supersedes` edge: an update) is left out of
`region`'s members, `nodes` and `search`, which say how many they left out in
`superseded_hidden`; `--include-superseded` (`include_superseded` on the
tools) lists it too, with its `superseded_by`. `get` always answers it, with
`superseded_by: [ids]`.

- `cauldron soup regions --env E --cauldron C`: every region with its id,
  handle, boundary, flags, the regions it narrows and relates to, and its
  member count.
- `cauldron soup region <R> --env E --cauldron C [--depth N]`: one region, the
  regions it narrows and that narrow it (to depth N, default 1), the regions
  it relates to either way, and its members, newest first.
- `cauldron soup nodes --env E --cauldron C [--region R] [--type T] [--state S]`:
  `{id, type, state, handle, regions}` per node, newest first (default 50);
  a region or viewer has no `state` key, since those types carry no state.
- `cauldron soup search <words> --env E --cauldron C [--region R] [--type T]`:
  each word of the query found case-insensitively, as a substring, in handles
  and bodies; ranked by terms matched, then handle over body, then newest; `{id, type, state, handle,
  regions, score, snippet}` (default 20).
- `cauldron soup get <id>... --env E --cauldron C`: nodes as records in the
  published shape, with their outgoing edges.
- `cauldron soup import <file-or-dir> --env E --cauldron C [--dry-run] [--remint] [--allow-duplicate]`:
  judges every record with the soup's own write door and lands the batch as
  one write, all or nothing. `--remint` treats ids as batch-local labels and
  prints the old-to-new map (`id_map`) when the batch lands or a dry run
  validates, never for a refused batch; a label may not equal an id the soup already
  holds (refused as `label-shadows-existing`), so a bare id that is not a
  label always means the existing node. A real import is refused while
  another import or recovery is writing to the same soup, or while an earlier
  import was interrupted and left the soup unsettled; `--dry-run` still works
  then, judged on a read-only snapshot. A batch identical to one that already
  landed in the soup (the same records, before re-minting) is refused as
  `batch-already-landed`, saying when it landed and how many records it held;
  `--allow-duplicate` lands a second copy on purpose. Every refused batch
  prints one shape: `outcome: "refused"`, a one-word `refusal`
  (`records-refused`, `landing-refused` or `batch-already-landed`),
  `accepted`, `refused` and `records` (empty when the batch was refused
  before any record was judged). Batches landed by
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
> /cauldron:curate kb --env prod The import failed because a record guessed a
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

It dry-runs with `cauldron soup import batch.json --env prod --cauldron kb
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
