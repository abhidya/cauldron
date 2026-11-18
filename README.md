# cauldron

![cauldron](docs/images/cauldron.png)

![cauldron teaser](docs/images/teaser.gif)

cauldron is a workspace for building software with coding agents. You don't
manage the agents one at a time. You dump what you know, what you want and what
you think of the result into one place. The questions and actions that come back
are sorted into the areas of the project they belong to.

Home: [github.com/broomstick-ai/cauldron](https://github.com/broomstick-ai/cauldron).
Developer: [samhaaf](https://github.com/samhaaf).

This is an early release. Some parts are working today and some are still
on the roadmap, and this README labels each one. [What is in this
release](#what-is-in-this-release) has the details.

## The picture

- **A cauldron is a project's knowledge store.** You keep one for each part
  of your mind: one for a big project, another for work, another for your
  family. They don't mix with each other: each has its own soup, its own
  dumps and its own history, private to you.
- **Cauldrons live in an environment, and you use `prod`.** An environment
  is an installed copy of the services (its own supervisor, data and
  versions); the installer makes `prod`, and your cauldrons go there. Other
  environments (`dev`, for example) are for developing cauldron itself. Name
  your cauldrons after what they hold; `prod` names the environment.
- **The soup is a knowledge graph.** Everything a cauldron knows goes into
  one typed graph, with structure, regions and discoverability, so agents
  can find context without having it pasted in again. You can write into it
  and read it today, from a shell and from Claude Code (see [From install to
  a working session](#from-install-to-a-working-session)). From 0.1.10 a
  ripple turns one dump into records when you ask (`cauldron ripples run`),
  and from 0.1.12 on its own, for a cauldron whose owner turned that on at
  a terminal (`cauldron ripples on`; off by default).
- **The surface is your view into the cauldron.** You dump ideas, feedback
  and half-formed thoughts into it. You collect text, pastes and references in the
  **bucket** and **dump** it onto the surface. Every dump is recorded in the
  **Ledger**, the cauldron's history of what came in and what ran.
- **Clusters are user-defined regions of the soup.** You name a cluster and
  describe in your own words what belongs in it; that description is its
  boundary. Each cluster gathers its own questions, actions, directives,
  constraints, objectives and milestones. Clusters can be nested: a cluster
  can sit inside another.
- **Ripples are short-lived agents that ripple out from your brain dumps,
  curate the soup and complete your tasks.** (0.1.10 has the first one, run
  by hand on one dump at a time; see [Ripples, by hand](#ripples-by-hand).
  From 0.1.12 it can also run on its own, for a cauldron whose owner turned
  that on; see [Ripples on their own](#ripples-on-their-own). The other
  ripples come later; see [ROADMAP.md](ROADMAP.md).)

## Why

Most of the time we no longer read the code our agents write. What we need
from the tooling has changed:

- **long-term consistency**, so that decisions made weeks ago still hold;
- **efficient context retrieval**, so that an agent starts with what it needs
  and nothing else;
- **knowledge organisation**, so that what you know is findable later;
- **task execution**, so that work moves without you supervising each step;
- **one interface**, where every question an agent has and every action a
  human must take (such as "set up this API key") rolls up into its cluster,
  instead of stalling in a chat window.

The goal is the simplest, strongest user experience for vibe coding. It runs
on brain dumps. You can always look underneath to see what is going on, but
the focus is design and intent, not the agents' output.

- **Questions wait at the cluster.** When an agent has a question for you, it
  waits in the cluster it belongs to. When you answer it, the tasks downstream
  of it resume.
- **Human actions wait there too.** When an agent needs a person for
  something ("go set up this API key"), an action waits at that cluster. Do
  it, and the tasks downstream of it resume.
- **The Ledger shows what happened beneath the surface:** every dump, and the
  chain of actions that resulted from it.

In this release, questions and actions already appear at their cluster, and
the Ledger is a time-ordered list of dumps, newest first, each expanding into
its phases and the work under it. Agents raising questions and actions, and
tasks resuming when they are answered, arrive with ripples, coming in 0.2.0
(see [ROADMAP.md](ROADMAP.md)).

## What holds it up

cauldron is built on **threads**, an engine (a daemon) that owns every agent
conversation and load-balances those conversations across a pool of Claude
Code accounts, so you make full use of your weekly limits:

- **Placement on every turn.** Each turn runs on an account that is under its
  limits. The limits are re-estimated continuously from observed usage, so
  the pool's weekly limits get used across all accounts instead of one
  account running dry while another sits idle. A turn that cannot be placed
  waits. It is never dropped.
- **Budgets.** A budget is a named meter that any set of threads can share. It
  covers the session and weekly limits as percentages, plus a dollar amount,
  and it applies to the accounts you name for it. One budget can cover one
  project, one account or the whole pool. That lets some work run continuously
  on a small slice while higher-priority work gets first call.

In this release, budgets are set through the threads daemon's API. Setting a
budget per cauldron from the UI is on the roadmap.

## Separate cauldrons for separate parts of your life

Keep one big soup per project, separate from the one for your work and the one
for your family. They don't mix: each cauldron's data is its own, under
`~/.cauldron/data/prod/cauldrons/<name>/`, and only you can read it (its
directories are mode 0700 and its files 0600; a cauldron made by 0.1.7 or
earlier is set so by its next write).

## What is in this release

The release is the workspace version (`version` in the top-level
`Cargo.toml`, the same in the plugin's `plugin.json`), and
[CHANGELOG.md](CHANGELOG.md) lists every change, newest first. On top of the V1 interface (two tab bars,
a first-run screen, the Ledger, the account pool, secret human actions and
`cauldron ctl`, from 0.1.2 and 0.1.3), the releases since made a cauldron's
soup a knowledge graph you use from Claude Code, and then let a ripple turn
your dumps into records:

- 0.1.4 and 0.1.5: the published soup schema, `cauldron create` and `list`,
  the `cauldron soup` verbs, `cauldron link`, and the Claude Code plugin with
  its curator.
- 0.1.6: the MCP door, a service through which Claude Code sessions read,
  search and write the cauldrons you grant to them.
- 0.1.7: dumps, raw text or a whole session transcript kept in a cauldron.
- 0.1.8: curating a dump into records (`/cauldron:curate-dump`), `cauldron
  rename` and `remove`, the plugin installed from GitHub, an upgrade that
  brings up the MCP service or moves it to the new build, a cauldron's data
  private to you, and fixes filed as issues.
- 0.1.9: removing one dump for good (`cauldron soup dump-remove`), and the
  MCP service hardened.
- 0.1.10: the first ripple, run by hand: `cauldron ripples run` turns one
  dump into unreviewed records ([Ripples, by hand](#ripples-by-hand)).
- 0.1.11: local inference, a model on this machine for the dump ripple,
  with nothing leaving it.
- 0.1.12: automatic ripples, turned on per cauldron by its owner and off by
  default ([Ripples on their own](#ripples-on-their-own)).
- 0.1.13: the review queue: see what a ripple wrote, grouped by dump, and
  approve or reject it in bulk at a terminal ([Reviewing a ripple's
  records](#reviewing-a-ripples-records)).

cauldron has three parts:

- **`cauldron`**, a command-line supervisor. It keeps each environment's
  services running under launchd (one supervisor per environment) and opens
  the terminal UI (`cauldron`, or `cauldron tui`).
- **`threads`**, the daemon described above.
- **`tui`**, the terminal UI. `cauldron` opens it.

All three are written in Rust. The repository also carries shell and Python
helper scripts, and an older Python client for the threads daemon. cauldron
runs on **macOS only** for now, and it is licensed under **Apache-2.0**.

**Working now:** environments; two tab bars, a global Config bar (TUI,
Devices, Providers, Accounts) over the environment bar (Cauldron, Threads,
Status, Permissions, Settings); a first-run screen that makes your first
cauldron; the cluster sidebar, with cluster creation from a boundary you
write; cluster tabs for questions, actions, tasks, nodes, status and details;
the bucket, and dumps, each of which is saved; the Threads tab, which shows
each environment's threads pinned or recent, read live from its threads
daemon; the Ledger, a time-ordered event flow of the environment's own dumps;
Status → Tokens, the real account pool and its usage pressure; Config →
Accounts, the threads daemon's accounts, masked as `a•••@e•••`, with login from
the face and each account switched on or off per cauldron; secret human
actions, typed into a masked input and routed to an env file or the macOS
keychain; the threads face, with persistent Claude Code conversations on the
account pool; `cauldron ctl`, which sends a dump, prints one frame of the
face or reads one soup node from the command line; the `cauldron-build`
demo environment (see below); and a cauldron's soup and dumps from the shell
and from Claude Code (see [From install to a working
session](#from-install-to-a-working-session)).

**Not yet:** the rest of the ripples. In the design, a dump sets off
**ripples**: agents that read it, update the soup, and turn it into tasks,
questions and actions. Today there is one, the dump ripple, which turns a
dump into records: you run it by hand (`cauldron ripples run`, from
0.1.10), or, from 0.1.12, it runs on its own for a cauldron whose owner
turned that on (`cauldron ripples on`; off by default, see [Ripples on
their own](#ripples-on-their-own)); or you curate a dump into the soup with
`/cauldron:curate-dump`. Questions and actions appear in the UI, but they
come from the environment's data or the demo data. See
[ROADMAP.md](ROADMAP.md).

## Requirements

- **macOS only.** The supervisor runs on launchd and the build signs its
  binaries with `codesign`. Linux and Windows are not supported.
- **A Rust toolchain**, installed with [rustup](https://rustup.rs) (stable,
  edition 2021), plus the Xcode Command Line Tools (`xcode-select --install`).
- **Claude Code**, installed and logged in (`claude` on your `PATH`). The
  threads daemon runs your agents through it.

## Install

```sh
git clone https://github.com/broomstick-ai/cauldron
cd cauldron
./scripts/install
```

`./scripts/install` builds `cauldron`, `tui`, the `threads` daemon and the
`mcp` service in release mode, makes the environment `prod`, pins `threads`
and `mcp` in it, and puts the environment's supervisor under launchd for the
current user. It always installs the whole stack. There are no prebuilt
binaries: it builds from the checkout. It installs
everything under your own home directory (`~/.cauldron`, or `$CAULDRON_HOME` if
you set it). It checks that it is running on macOS before it does anything, and
it prints each step it takes. It does not sign anything with a personal
identity.

Run it from a clean checkout. A first install builds `threads` and names that
build by its git commit, so it stops before building anything if the checkout
has uncommitted or untracked files, and lists them. Commit, move or delete them,
then run it again. If an install stops partway, fix what it reports and run
`./scripts/install` again: it finishes the steps the first run did not.

To see the plan without changing anything, run `./scripts/install --dry-run`.
If a cauldron supervisor is already running for that environment, the install
stops and prints the command to replace it
(`./scripts/install --replace-running`).

**Upgrading.** After a `git pull`, run `./scripts/install --replace-running`
from a clean checkout (the upgrade builds the `mcp` service, and a build
needs one). An environment installed before the MCP service existed (0.1.5
or earlier) has no `mcp` pin; the upgrade builds the service and pins it. An
environment whose `mcp` pin is an older build (0.1.6 or 0.1.7) has it
rebuilt from this checkout and re-pinned; the supervisor moves the running
service to the new build, and your grants are kept. A pin that is already
this checkout's build is left as it is, so a second run changes nothing.
Without `--replace-running` an older `mcp` pin is left as it is, and the
install says plainly that the mcp service is still the old build and prints
the two commands that upgrade it:

```sh
cauldron --env prod build mcp --env-copy
cauldron --env prod pin mcp <the label that build printed>
```

`cauldron mcp status` shows the running service's version beside the
`cauldron` command's, and says when they differ; so does a Claude Code
session (the service's instructions, and any refusal of a tool or an
argument the older service does not know). `--dry-run` shows the plan.

The install puts `cauldron` in `~/.cauldron/bin` and does not change your
`PATH`; its last lines say so. Add that directory to your `PATH` once. For zsh:

```sh
echo 'export PATH="$HOME/.cauldron/bin:$PATH"' >> ~/.zshrc
```

Then open a new terminal. (For bash, use `~/.bash_profile`. If you set
`$CAULDRON_HOME`, the directory is `$CAULDRON_HOME/bin`.) The commands below
assume `cauldron` is on your `PATH`.

Claude Code must find `cauldron` too: the plugin starts its MCP server as the
bare command `cauldron mcp stdio`. Claude Code started from a terminal takes
that terminal's `PATH`; started from the Dock, Finder or an IDE, it may not
read your shell startup file, and its sessions then have no cauldron tools.
Start it from a terminal where `command -v cauldron` prints
`~/.cauldron/bin/cauldron`.

To check that the services are up:

```sh
cauldron status
```

## First run

```sh
cauldron
```

This opens the terminal UI. Press `?` to see every key binding. On an empty
install it opens on one action, **+ create your first cauldron**, and makes
nothing until you take it. After that, `cauldron` opens the cauldron you used
last, unless you name one. To see every part of the UI with something in it,
open the [`cauldron-build` demo
environment](#the-cauldron-build-demo-environment).

**More cauldrons.** Keep one cauldron per project, all in `prod`:

```sh
cauldron create myproject --env prod
cauldron list --env prod
```

Environments other than `prod` are for developing cauldron itself (`cauldron
env create dev`); `cauldron envs` lists them, and
[`docs/environments.md`](docs/environments.md) covers their lifecycle.

**Your first cluster.** A cauldron with no cluster of its own opens with the
Clusters sidebar's `+` selected: press Enter to make a cluster at the root, or
`+` beside an existing cluster to make one under it. Give it a
name, then describe in your own words what belongs in it. That description is
the cluster's boundary, and cauldron uses it to seed the cluster from what the
environment already holds.

**Talking to an agent directly.** `cauldron tui --chat` opens the threads face
on the threads daemon. Start a thread, give it a task, close the terminal, and
it is still there, in the same place, when you come back. The Threads tab in
`cauldron` shows the same threads, pinned or recent, for the environment you
are in: open one to read it, send it a message, or look inside a tool call or
a subagent. Starting a new thread is done from the threads face for now.

## The `cauldron-build` demo environment

`cauldron-build` is a curated environment. Its data is cauldron's own
development, scrubbed of private names, paths and credentials. It shows every
part of the UI with something in it:

- clusters, and the aggregate cluster;
- directives, constraints, objectives and milestones;
- questions, and multi-step actions, including a secret action;
- the Ledger;
- opening a file from a node.

The data ships in the soup crate, not in the UI code, and no other environment
shows it. Open it with:

```sh
cauldron env create cauldron-build
cauldron --env cauldron-build
```

## From install to a working session

This is the whole path, from a fresh Mac to a Claude Code session writing
into a cauldron. Every command names `--env prod`: see the `$ENV` note
below for why.

1. **Install** (see [Install](#install)): `git clone
   https://github.com/broomstick-ai/cauldron`, then `./scripts/install` in
   the checkout. It makes the environment `prod` and runs its `threads` and
   `mcp` services.
2. **Put `cauldron` on your `PATH`** (`~/.cauldron/bin`), open a new
   terminal, and check with `command -v cauldron` and `cauldron status`.
3. **Make a cauldron** for your project:

   ```sh
   cauldron create kb --env prod
   ```

   Its soup starts empty, with no regions, and the curator files nothing it
   cannot place in a region.
4. **Give it regions.** The plugin ships an example,
   [`claude-code/cauldron/regions.example.json`](claude-code/cauldron/regions.example.json):
   four regions (the code, its tests, the product, the process). Copy it,
   edit the handles and boundaries to fit your project, then judge it and
   land it:

   ```sh
   cauldron soup import regions.json --env prod --cauldron kb --remint --dry-run
   cauldron soup import regions.json --env prod --cauldron kb --remint
   ```

   (Or skip this step: on the first run the curator proposes regions for you
   to approve.)
5. **Open the cauldron to Claude Code**, yourself, at a terminal (an agent
   cannot do this):

   ```sh
   cauldron mcp grant kb --env prod --project ~/code/proj
   ```

   Every cauldron is closed to Claude Code until you grant it. `--project`
   makes `kb` the default for sessions working in `~/code/proj`.
6. **Install the plugin** inside Claude Code:

   ```text
   /plugin marketplace add broomstick-ai/cauldron
   /plugin install cauldron@cauldron
   ```

   Then restart Claude Code from a terminal where `cauldron` is on the
   `PATH`. In a session, `/mcp` lists `cauldron` as connected.
7. **Write into the cauldron.** In a session in `~/code/proj`:

   ```text
   /cauldron:curate Always read region ids from the soup before writing records: a guessed id made an import fail.
   ```

   The command asks the MCP door which cauldron this directory means (`kb`,
   from the grant's project), tells you, and asks you to confirm before it
   writes to `prod`. The curator then judges its records (a dry run), lands
   them, and reports their new ids.

> **Two ways to say "this directory means cauldron `kb`".** They are
> separate, and setting one does not set the other.
>
> | | `.cauldron` marker | grant `--project` |
> |---|---|---|
> | made by | `cauldron link kb --env prod` | `cauldron mcp grant kb --env prod --project <dir>` |
> | read by | the `cauldron soup` commands (and the curator's shell fallback) | Claude Code sessions, through the MCP door |
> | opens the cauldron to Claude Code? | no, never | the grant does; `--project` only sets the directory's default |
> | kept in | a file in the project (keep it out of git) | `~/.cauldron/data/prod/mcp/grants.json` |
>
> A session uses the grant's project; a shell uses the marker. For both,
> run both commands.

> **`$ENV`.** `cauldron create`, `cauldron mcp grant` and the other
> commands take their environment from `--env`, else `$CAULDRON_ENV`, else
> `$ENV`, else `prod` (the `cauldron soup` commands never fall back to
> `prod`: they need one of the first three, or a marker). Some shells export `ENV`
> for their own use (`ENV=development`, or a startup file's path); then
> `cauldron create kb` without `--env` aims at that environment (and is
> refused when no such environment exists), while Claude Code sessions,
> whose bridge never reads `$ENV`, use `prod`. Pass `--env prod`, as every
> command here does, or unset `ENV`.

**What the door gives a session.** The plugin's MCP server, `cauldron mcp
stdio`, is a bridge to the environment's `mcp` service. A session gets tools
to read, search and write a granted cauldron's soup (`soup_status`,
`soup_regions`, `soup_region`, `soup_nodes`, `soup_search`, `soup_get`,
`soup_put`, `soup_link`, `soup_changes`) and its dumps (below). The grants:

```sh
cauldron mcp grant kb --env prod --project ~/code/proj   # open kb; sessions in ~/code/proj use it by default
cauldron mcp grants --env prod                          # what is open
cauldron mcp revoke kb --env prod                       # close it again, from the next call on
cauldron mcp status --env prod                          # the service and what it holds
```

A grant opens the cauldron to the Claude Code sessions you run on this
machine, whichever directory they work in: any of them can name it.
`--project` only tells a session which cauldron a directory means when the
session does not name one; it does not limit which sessions can reach the
cauldron. `grant` refuses to run without an interactive terminal, so an
agent cannot open a cauldron for itself, and no MCP tool can grant or
revoke. `--scopes read` opens a cauldron read-only. While the service holds
a granted cauldron it is the soup's one writer: `cauldron soup import` is
sent to it, and `cauldron soup recover` is refused.

**From a shell.** `cauldron soup import | regions | region | nodes | search
| get` write records into a soup and read them back (`cauldron soup help`).
The records follow the published schema in
[`schema/soup/workspace-v6/`](schema/soup/workspace-v6/README.md), whose
README also says how to change a node (supersede it), what the import checks
and what it leaves to you. `cauldron link kb --env prod` writes a
`.cauldron` marker into a project directory, so `cauldron soup` run inside it
needs neither `--env` nor `--cauldron`.

**Dumps, and curating them.** A dump is raw text put into a cauldron for
later: a note, a file, or a whole Claude Code session's transcript. It is
kept in the cauldron's dumps store beside its soup (not in the soup), as
`DM-<n>` in state `received`. From Claude Code, `/cauldron:dump` keeps text
or a file and `/cauldron:dump-session` keeps the session's transcript (the
grant's `dump` scope allows both); from a shell, `cauldron soup dump <file|->
--env prod --cauldron kb`. `/cauldron:curate-dump DM-3` has the curator read
the dump in pages (for a transcript, only what you typed and what the
assistant wrote as text: tool calls and their output, thinking, `!` shell
commands and their output, local commands' output, slash-command wrappers,
system reminders and attachments are left out and counted) and land the
records worth keeping, each citing the dump; the dump itself stays
`received`. `cauldron soup
dump-get DM-3 --env prod --cauldron kb --view text` shows the same pages in
a shell. A session transcript holds tool output, shell output and anything
pasted into the session, and it is stored verbatim (the raw view, `dump-get`
without `--view text`, shows all of it), in your own cauldron, readable only
by you. The grant's scopes govern Claude Code sessions; the `cauldron soup
dumps` and `dump-get` commands run as you and read dumps without a grant.

**Renaming and removing a cauldron.** `cauldron rename kb notes --env prod`
renames it; its soup, dumps and grant follow. `cauldron remove kb --env
prod` is a dry run; with `--yes` it moves the cauldron to
`cauldrons/.removed/` (it never deletes) and drops its grant. Both are
refused while the `mcp` service holds the cauldron: stop the service
(`cauldron --env prod stop mcp`, then `start mcp` afterwards) or revoke the
grant first (a revoked cauldron has to be granted again). A project's `.cauldron` marker naming the old name is not
updated; run `cauldron link notes --env prod --replace` there.

The plugin's [README](claude-code/cauldron/README.md) has the details.

### Ripples, by hand

A **ripple** turns one dump into new records in the soup: ideas, issues,
questions, decisions, constraints, tasks and dangers, each one claim, each
citing the dump it came from. A ripple runs when you run it, on the one dump
you name, or, for a cauldron where you turned that on, on each new dump by
itself ([Ripples on their own](#ripples-on-their-own)):

```sh
cauldron ripples run DM-3 --env prod --cauldron kb        # one dump; prints the records and the tokens
cauldron ripples status --env prod --cauldron kb          # the last runs, spend today and per dump, the caps
cauldron ripples review --env prod --cauldron kb --text   # what a ripple wrote and nobody reviewed yet, by dump
cauldron ripples approve --dump DM-3 --env prod --cauldron kb   # you, at a terminal: mark a dump's records reviewed
cauldron ripples reject --dump DM-3 --env prod --cauldron kb    # you, at a terminal: keep them out of the reads
```

**Off unless you start it.** The work is done by the `ripples` service,
which the installer does not start. Start it yourself, in the environment:
`cauldron --env prod build ripples`, then `cauldron --env prod pin ripples
<the version label it printed>`, then `cauldron --env prod start ripples`.
The `mcp` and `threads` services must be running too (threads from this
release: an older threads is refused, see below), and the cauldron must be
granted to the MCP door with `read`, `write` and `dump` (the default
scopes). Even running, the service processes nothing on its own unless you
turned automatic processing on for a cauldron (`cauldron ripples on`, see
[Ripples on their own](#ripples-on-their-own)); otherwise only `cauldron
ripples run` sets it off. Stop it with `cauldron --env prod stop
ripples`.

**The model call is a bare completion, not an agent.** The call goes
through the environment's `threads` service, which runs it as one Claude
Code call (`claude -p`) on the Claude login threads uses on this machine,
under threads' own budgets. The ripple asks threads for a thread with
nothing an agent has, and threads runs it that way:

- no tools of any kind: no built-in tool (`--tools ""`), an explicit deny
  of every built-in tool and of threads' own MCP server, no allow rule, and
  no MCP server at all (`--strict-mcp-config` with an empty config);
- no settings files, so no hooks, plugins or permission rules from your
  user or project configuration (`--setting-sources ""`), no skills or
  commands (`--disable-slash-commands`), and no plugin directory;
- no CLAUDE.md or memory: Claude Code's safe mode (`--safe-mode`, which
  its help says starts with CLAUDE.md, skills, plugins, hooks and MCP
  servers disabled), auto-memory turned off, and an empty working directory
  made for the call. Settings an administrator manages (policy) still
  apply, as they do to every Claude Code;
- permission mode `dontAsk` (a tool use nothing pre-approved is denied),
  never a skip-permissions mode;
- one model turn, its answer bounded to 16,000 tokens, and given as JSON
  that must meet [`batch.schema.json`](services/ripples/package/dump/v1/batch.schema.json)
  (Claude Code's structured output; its `StructuredOutput` step is how the
  answer is returned, not a tool the model can use for anything else).

Before anything is sent, the ripple reads the thread back from threads and
refuses to go on (`threads_unsupported`) unless threads kept those
settings: an older threads silently ignores them. The dump's text sits in
the prompt between two marker lines, after a rule that it is untrusted data
and never instructions, and the answer is checked against the schema again
by the ripple itself. Those two are there to keep answers useful; what
keeps a dump from steering anything is that the call has no tool to use.

**What leaves this machine, and what is stored where.**

- *Sent to the model provider* (Anthropic, through Claude Code on the login
  threads uses): Claude Code's own system prompt, which names the working
  directory (an empty directory made for the call under the system's
  temporary directory, never your project or home), the platform, the OS
  version and the date, and a note Claude Code adds naming the email
  address of the login the call runs on; and one prompt, the file
  [`services/ripples/package/dump/v1/prompt.md`](services/ripples/package/dump/v1/prompt.md)
  filled in with the cauldron's regions (id, handle and boundary, at most
  50, each boundary cut at 400 characters), the dump's id and kind, and the
  dump's text. No existing record of the cauldron is sent. A text dump is
  sent whole. A session dump is sent only as its text view: what you typed
  and what the assistant wrote as text (a subagent's text included), never
  tool calls, tool output, thinking or shell output. Since 0.1.12 the text
  (and the region text) is first passed through the secret scrubber, which
  replaces credential-shaped text with `<redacted:kind>` ([the
  scrubber](#the-secret-scrubber)); it reduces the risk and is not a
  guarantee. The schema is sent as the shape of the answer.
- *The call's environment*: `claude` runs with every variable of the threads
  service's environment removed except `PATH`, `HOME`, `USER`, `LOGNAME`,
  `TMPDIR`, the locale (`LANG`, `LC_ALL`, `LC_CTYPE`) and proxy settings
  (`HTTPS_PROXY`, `HTTP_PROXY`, `NO_PROXY`, `NODE_EXTRA_CA_CERTS`), plus
  the login's `CLAUDE_CONFIG_DIR` and settings that turn off Claude Code's
  telemetry, error reports and other non-essential traffic. These stay on
  this machine; Claude Code needs them to run and to find its login.
- *Kept by threads* (in the environment's threads store,
  `~/.cauldron/data/<env>/threads/store/`): one thread per call, named
  `ripple <first characters of the run id>`, with its settings; the prompt,
  dump text included, as the thread's message, and the answer and its token
  counts as events, in `threads.db`; and the call's output stream (the
  answer, never the prompt) in `turns/<thread>/`. threads keeps them until
  you delete them; nothing in this release removes them, and removing the
  dump afterwards (`cauldron soup dump-remove`) does not either.
- *Not kept by Claude Code*: the call runs with `--no-session-persistence`,
  so no session transcript is written under the login's `projects/`
  directory.
- *Kept by the ripple*: the run journal,
  `~/.cauldron/data/<env>/ripples/runs.jsonl`, with each step's ids,
  counts, models, tokens and outcome: codes and fixed reasons, never dump
  text, the prompt, the model's answer or its notes, or a backend's own
  words. `run` shows you those words on the terminal (`detail`, `notes`)
  and writes them nowhere; `cauldron ripples run … --debug-keep-answer`
  keeps one run's answers and refusals, unscrubbed, in one owner-only file
  under `~/.cauldron/data/<env>/ripples/debug/`, whose path it prints (by
  hand only; delete it when done). The call's working directory is removed
  when the call ends.
- *Written to the soup*: the records, through the `mcp` service, audited in
  its `calls.jsonl` (counts and ids, never text).
- *With the local backend* (`cauldron ripples run ... --backend local`, see
  [Local inference](#local-inference)): **nothing leaves this machine.** The
  same prompt and schema go over loopback to the environment's `inference`
  service and from it to its `llama-server` child on 127.0.0.1; threads,
  Claude Code, a login and the network are not involved, so no provider
  receives anything and threads keeps nothing. Every address the ripple
  dials must be a loopback address, or nothing is sent. The `inference`
  service writes no prompt or answer itself; its `llama-server.log` holds
  llama-server's own output at its default log level, which has not yet
  been checked against a real model. The ripple's journal gains a `local`
  line per call: the model, the sha256 of the model file as checked at
  load, the tokens and tokens per second, still never text.

The scrubber does not catch every secret, so do not run a ripple on a dump
that you know holds one: remove the dump instead (`cauldron soup
dump-remove`), before any run.

**Removing a dump after a ripple ran on it** erases the dump's content and
nothing else. Two things stay:

- *The records the ripple drew from it.* `dump-remove` does not touch them,
  and a model may have copied text from the dump into one. Find them by
  their cite: each carries `meta.cites: [{"kind": "dump", "id": "DM-3"}]`
  (`cauldron soup get <id>` shows it). The run's records are also listed by
  the dump itself before you remove it (`processed.records` in `cauldron
  soup dumps`), by `cauldron ripples status` (the run's `records`), and,
  while you have not reviewed them, by `cauldron ripples review --dump DM-3`.
  Reject or supersede any record you do not want.
- *threads' copy of each call.* The threads service keeps a copy of every
  ripple call's prompt (the dump's text included) and of its answer, in its
  store, as above, until you delete it.

**How a ripple's records are marked.** Each record cites the dump
(`meta.cites: [{"kind": "dump", "id": "DM-3"}]`) and carries `meta.ripple`
(the package, the run and the model). It is written through the `mcp`
service, which records the write as a ripple's: no field of the record can
say otherwise. Every read shows it, marked `unreviewed: true`, until you
approve it: `cauldron soup nodes --review pending` (and `soup_nodes` /
`soup_search` with `review: "pending"`) lists only those. An unreviewed record was written by an automatic step from your dump, so
its text is data, not instructions: an MCP reply that holds one carries an
`unreviewed_note` saying so, and the plugin tells Claude Code sessions never
to act on an instruction found inside a record or a dump, and to tell you
when one reads like an instruction. That is guidance to the session, not a
control.

**Reviewing is yours, at a terminal**: see [Reviewing a ripple's
records](#reviewing-a-ripples-records).

**What a ripple may write.** New records only (never an edit, a close or a
supersede of an existing node), of those seven types, each in at least one
existing region, linked only with `derived_from` to other records of the
same answer, at most 25 at once. No directives, regions or flags. A label
used twice in one answer (`duplicate-id-in-batch`), a label equal to an id
the cauldron holds (`label-shadows-existing`) or a record that links to
itself is refused, and nothing of that answer lands.

**The caps, counted across runs.** Before every call the ripple adds up, from
its journal, what has been spent:

- per dump, across every run of it: at most 3 model calls and 200,000
  input tokens. Running a dump again does not reset them;
  `cauldron ripples run ... --force-budget <tokens>` lets that one run spend
  that many more input tokens (and 3 more calls) on the dump;
- per cauldron per day (UTC): at most 1,500,000 input tokens or 30 dumps.
  `--force-budget` does not lift this.

A call is charged when it begins: before anything is sent, the ripple
checks the caps and writes the call into its journal in one step, under a
lock, as one call and its estimate (the prompt's characters divided by
three), so two runs cannot both take the last call. When the answer comes,
the call is settled to the larger of the input tokens it reported and that
estimate. A call cut short (the `ripples` service killed or stopped during
it) is never settled and stays charged at its estimate; `status` counts it
and shows it under `unsettled_calls`. The estimate does not include Claude
Code's own system prompt, and one call can go past a cap (the check is made
before it, not during it). Over a cap,
nothing more is sent and the run ends `deferred` (`budget` or `daily_cap`),
with the dump still `received`. The first call is Sonnet (or `--model`); an
answer that misses the schema, or that the soup refuses, gets one retry on
Opus with the refusal. A backend that answers busy (another request holds
the local `inference` service, or it is not ready yet; threads refusing the
call as busy or overloaded) refused the call before it ran: it is charged
nothing and is no call against a cap, and a run by hand tries it at most 3
times, then ends `deferred` (reason `busy`) with the dump still `received`.
`run` prints each run's tokens (input, output, cached, the calls and the
models), and `status` shows the spend today per cauldron and per dump. Today's
spend is grouped by the cauldron itself (its id), not by the name it was
spent under, and shown under its current name, with `cauldron_id` and the
`names` its calls were journaled under: a renamed cauldron's day is one
entry, a cauldron made again under an old name is another, and spend of a
cauldron no longer in the environment shows as `<name> (gone)`. The
evaluations' ledger (`evaluations_today`) is grouped the same way.

**A call given up is stopped.** When threads parks the call under its
budget, the turn ends without an answer, or no answer comes within 15
minutes, the ripple asks threads to stop the turn and to withdraw the
message (so no budget reset or retry runs it later), then reads the thread
until it is idle. A call whose `claude` ends without an answer is not run
again: threads ends it at once with the last lines `claude` wrote to its
error output (the home directory shown as `~`, the call's working directory
as `<cwd>`), and the run ends `inference_failed` with that text. The
outcome says whether threads confirmed it; if not, it
says the call may still be running and how to stop it (cancel that thread
from a threads face, or stop the threads service), and `status` lists it
under `possibly_running`. A call left running by a killed or stopped
`ripples` service is cancelled the same way when the service next starts
or runs.

**The dump.** A dump a ripple processed moves from `received` to
`processed`, and records which run and which records (`soup_dumps` and
`dump-get` show it). Curating by hand (`/cauldron:curate-dump`) still leaves a
dump `received`. A removed dump, or one whose removal is unfinished, is
refused. Running a ripple on a dump twice gives one set of records: the
second run answers the first one's. A run cut short (the service stopped or
killed) shows as `interrupted` in `cauldron ripples status`; run the same
command again, and it lands once.

### Reviewing a ripple's records

New in 0.1.13. Every record a ripple writes is **unreviewed** until you
decide it: you approve it (it reads as reviewed from then on) or you reject
it (it is kept, and left out of the normal reads). Only you decide, at an
interactive terminal; a ripple, a Claude Code session and every MCP tool
can only read.

**The `mcp` service must be this release's.** A decision is written by the
running `mcp` service, and this command line refuses to send one to an
older service (it would not read the decision as this release means it),
writing nothing. If you upgraded without `--replace-running`, move the
service to the new build first, with `./scripts/install --replace-running`
or the two commands under "Upgrading" above.

```sh
cauldron ripples review --env prod --cauldron kb --text          # the queue, grouped by dump and run
cauldron ripples review --env prod --cauldron kb --dump DM-3     # one dump's records, as JSON
cauldron ripples approve --dump DM-3 --env prod --cauldron kb    # every record of that dump
cauldron ripples reject --run <run> --env prod --cauldron kb --reason "duplicates"
cauldron ripples approve C-12 IS-4 --env prod --cauldron kb      # by id
cauldron ripples review --interactive --env prod --cauldron kb   # walk the queue, a key per dump
```

**1. See what waits.** `cauldron ripples review` lists the records a ripple
wrote that carry neither decision, grouped by the dump they cite and the run
that wrote them, newest group first. Each group says the model and backend
that wrote it, when, and the exact commands that approve or reject it; each
record its id, type, state, handle, the first 240 characters of its body,
its regions and its other edges. `counts` gives the records waiting, the
groups, per dump and per type, and how many of the matching records you
already approved or rejected. Filters: `--dump DM-n`, `--run <run>`, `--type
<type>`, `--since <YYYY-MM-DD or RFC 3339 time>`. The answer is at most
`--max-chars` characters (default 12,000, at most 50,000; under 2,000 is
refused), as JSON and as `--text`: whole records while they fit, and a
record longer than the bound cut (its edges, then its longest fields),
listed in `truncated`. One that stopped early says `next_after`, the last
record it showed; `--after <that id>` lists the records after it, so you can
decide a page and then ask for the next without skipping any. `--text`
prints it for reading, each record on one line, with its control and
invisible characters (escape sequences, bidirectional overrides, zero-width
characters) shown escaped, as `\u{202E}`, so a record cannot draw on your
terminal or reorder what a line shows; a group cut by the page says how
many of its records it shows, and that its commands decide them all. It
reads only, so it works with the services running or not. `cauldron ripples status`, `cauldron mcp status` and `cauldron status`
show how many records wait per cauldron, with this command; a dump receipt
that waited for its records (`wait_secs`) names it too, and the approve and
reject commands for that dump.

**2. Decide.** `cauldron ripples approve` and `cauldron ripples reject` take
the records one way: `--dump DM-n` (every record a ripple wrote citing that
dump), `--run <run>` (every record that run wrote), or record ids. Each is
one write through the running `mcp` service, as the owner, audited in its
`calls.jsonl` (`soup_approve`, `soup_unapprove`, `soup_reject` or
`soup_unreject`, with the selector, the counts and, for `--run`, the run;
never a record's text or your reason).

- **Approve** sets `meta.approved: {at, by: "owner"}`. `--unapprove` takes
  it back.
- **Reject** sets `meta.rejected: {at, by: "owner", reason}` (`--reason`:
  one line, at most 500 characters). Nothing is deleted. `soup get` (and
  `soup_get`) still answers a rejected record, marked `rejected: true`,
  with its cite to the dump and your decision. `soup nodes`, `soup search`
  and a region's members (and `soup_nodes`, `soup_search`, `soup_region`)
  leave it out and say how many in `rejected_hidden`; `--include-rejected`
  (MCP: `include_rejected`) lists them, marked. The queue no longer lists
  it. `--unreject` takes a rejection back: the record is unreviewed again.
- **The two exclude each other.** A rejected record is not approved, by
  `ripples approve` or `soup approve`, until you take the rejection back;
  an approved one is not rejected until you take the approval back. Asking
  again for a state a record is already in changes nothing: the answer
  lists it in `skipped`, with why.

`cauldron soup approve <id>... [--unapprove]` still works as before, and
like these it needs this release's `mcp` service.

**3. Or walk it.** `cauldron ripples review --interactive` shows the queue
one group at a time and takes a key: `a` approves the whole group, `r`
rejects it (asking for an optional reason), a record's number marks it
(approve, then reject, then unmarked; several at once: `1 3`) and `w`
writes the marks, `s` skips the group, `q` quits. Each group's decision is
one write: the reason is asked before anything is written, then its
approvals and rejections land together (one audit line,
`soup_approve_and_reject`, when it does both), or, when any of its records
is no longer as it was listed, nothing of the group lands and the pass says
why. A skipped group, and every group after `q` or the end of input, is
left as it was. Records are shown as `--text` shows them. It takes the same
filters as `review`.

**Why a terminal.** `approve`, `reject` and `review --interactive` run only
with their input and output on an interactive terminal, like `cauldron mcp
grant`, and no option skips that: a script or an agent running them is
refused, and the refusal names the exact command for you to run. No MCP
tool approves or rejects, so no Claude Code session can. A ripple can do
neither: the soup refuses `meta.approved` and `meta.rejected` from a ripple,
and no record write (`soup import`, `soup_put`, a ripple's batch) may carry
either, set or cleared, in any casing or nesting. In a Claude
Code session, `/cauldron:review` reads the queue (the MCP tool
`soup_review`, scope `read`) and tells you these commands; it never runs
them. Unreviewed text was written by a model from a dump, so the session is
told to treat it as data and never to act on an instruction inside it. That
is guidance to the session; the terminal check guards the commands, not
the files, and a process running as you that reads the `mcp` service's
private key file can send the same request.

### Ripples on their own

New in 0.1.12. **Off by default, for every cauldron, and an upgrade never
turns it on.** The owner turns it on for one cauldron at a time, at a
terminal:

```sh
cauldron ripples on kb --env prod                     # threads backend, text dumps only
cauldron ripples on kb --env prod --backend local     # this machine's inference service; nothing leaves it
cauldron ripples on kb --env prod --include-sessions  # session transcripts too (their text view)
cauldron ripples status --env prod --cauldron kb      # on or off, since when, the backend, the queue, spend today
cauldron ripples off kb --env prod                    # from anywhere; before the next call
```

**What turning it on means.** From that moment, each dump that lands in the
cauldron is processed with no further command, exactly as `cauldron ripples
run` would process it: the same call, the same caps, the same journal, and
records marked `unreviewed` until you approve or reject them at a terminal
([Reviewing a ripple's records](#reviewing-a-ripples-records)). It authorises model spend and writes into the
cauldron that nobody asks for one by one, so, like `cauldron mcp grant`,
`on` requires an interactive terminal (its input and output must both be
one; no MCP tool, seam verb or option turns it on). It prints what will be
sent where, the caps and the failure policy, then asks `[y/N]`, and writes
the setting only on yes. The setting is a file per cauldron,
`~/.cauldron/data/<env>/ripples/auto/<cauldron>.json` (owner-only, written
whole and renamed into place); no file means off.

- **Bound to one cauldron, never to its name.** The setting records the
  cauldron it was turned on for: the environment and the cauldron's id, the
  random id in its `cauldron.toml` that its MCP grant is bound to too (see
  [The cauldron's id](#the-cauldrons-id)). It is checked before every
  automatic run and every model call of one. If the cauldron is gone, a
  cauldron made again under its name is there (even one whose
  `cauldron.toml` names the same creation time), or the file was copied
  from another cauldron or environment, the setting is void: nothing runs,
  a dump's receipt says it is not taken, and `status` shows `automatic:
  void`, why, and how to clear it (`cauldron ripples off`). The cauldron's
  directory plays no part once it has an id, so a data root moved whole,
  to another place or another volume, keeps its settings on, and so does a
  copy of a whole data root restored elsewhere. `cauldron remove` turns automatic processing off for the cauldron
  and says so; `cauldron rename` turns it off too and prints the `on`
  command for the new name (an authorisation of automatic spend never
  follows a rename). A cauldron made again under an old name starts with a
  clean queue.

- **Only new dumps.** `on` records the newest dump the cauldron holds, and
  only dumps after it are processed automatically. Older dumps are
  processed only by hand. Turned off and on again, the mark moves to that
  moment.
- **Text dumps only, unless `--include-sessions`.** A session dump is then
  sent as its text view, as by hand.
- **The service.** The `ripples` service must be running; the installer
  does not start it (see [Ripples, by hand](#ripples-by-hand) for how).
  The `mcp` service tells it when a dump lands through it; it also looks for
  dumps still `received` when it starts and every minute, which finds dumps
  that landed while it was down and dumps `cauldron soup dump` wrote
  straight to the store. One dump at a time, oldest first, one dump per
  cauldron per pass, the cauldron served least recently first. One service
  per environment: a second one started on the same data refuses to start
  and names the first's pid. While an automatic run holds the service's
  one run slot, `cauldron ripples run` is refused `busy` with `held_by:
  automatic`; the slot is let go between dumps, so trying again in a moment
  gets it. While a run by hand or an evaluation holds it, a dump that lands
  waits, `status` says so (`waits_for.run_slot`), and it starts when that
  run ends.

**What is sent where, per backend.**

- *threads* (the default): the same bare completion as by hand, on this
  machine's Claude login (see [What leaves this machine](#ripples-by-hand)
  above): the dump's text, scrubbed, with the cauldron's regions, to
  Anthropic; threads keeps a copy of each call's prompt and answer in its
  own store. Sonnet first, one Opus retry when the answer is refused, or
  `--model`.
- *local* (`--backend local`): the `inference` service on this machine,
  over loopback; nothing leaves the machine. The text is scrubbed too,
  unless `on` was given `--no-scrub-local`.

**Caps.** As by hand, charged before each call: per dump, 3 calls and
200,000 input tokens across every run of it; per cauldron per UTC day,
1,500,000 input tokens or 30 dumps. Over a cap the dump stays `received`,
its listing shows `auto: {state: deferred, reason: budget, retry_after}`,
and it is tried again after midnight UTC, when the daily window resets (a
dump over its own cap is then deferred again with nothing sent, until you
run it by hand with `--force-budget`). The daily cap is counted per
cauldron, not per name: a cauldron made again under an old name starts its
day clean, and a renamed one keeps what it spent that day.

**When threads parks the call.** threads holds a call when every account it
may use is over its usage threshold, an account reached its limit, the
credit balance is spent, the login expired or one of its budgets tripped.
The ripple withdraws the call; the dump stays `received` with `auto:
{state: deferred, reason: threads_parked, park: <cause>, retry_after}`,
and the attempt is not a failed one. A call threads parked before it ran
(no tokens reported) is charged nothing and does not count against any
cap. After a park, no automatic run on threads, of any cauldron in the
environment, starts until the wait is over: 2 minutes, then 8, then 32,
then an hour at most, for as long as the parks go on in a row. A parked
account costs one withdrawn call per wait, not one per dump. `status`
shows the hold (`automatic.threads_hold`: since when, the cause, the dump
that set it, the next attempt) and marks each cauldron it holds (`held`).
When the wait is over, the cauldron served least recently is tried first,
so one whose calls keep being parked does not keep the others waiting.

**When the backend is busy.** A call the backend refused as busy (another
request holds the local `inference` service, as a run by hand or another
client does, or it is not ready yet; threads refusing it as busy or
overloaded) ends the automatic attempt at once: the dump stays `received`
with `auto: {state: deferred, reason: busy, retry_after}`, the attempt is
not a failed one and is charged nothing, and the next scan tries it
again.

**Failure.** A failed attempt (the model could not be reached, the call
failed or ended at once, as a `claude` that refuses its command line and
exits does, the service was stopped during it) leaves the dump `received` with
`auto: {state: retrying, failed_attempts, last_code, retry_after}`, and it
is tried again after 2 minutes, then 8. After 3 failed attempts, or at
once when trying again cannot help (the answer was refused after its retry,
the cauldron has no region, threads is too old, the scrubber ran out of
time), the dump is parked in state `failed` with `failure: {code, message,
attempts, what_to_do}`, whose message is a fixed sentence for the code. It
is never tried automatically again; `cauldron ripples run` still runs it by
hand, and a dump processed that way no longer lists the failure. Each
attempt is held to the dump's caps, so a failing dump costs at most what
any dump may. Every attempt and outcome is a line in
`~/.cauldron/data/<env>/ripples/auto.jsonl`: ids, codes, counts, times and
fixed reasons, never the dump's text, the model's answer or a backend's own
words (nor the end of a failed `claude`'s stderr that threads reports; a
run by hand shows it on the terminal). If that file cannot be written,
nothing is run until it can.

**Seeing it.** `cauldron ripples status` shows, per cauldron with a
setting: on, off or void, since when, the backend, and the queue (waiting,
running, deferred with the reason, retrying, failed with the reason,
session dumps not taken, the last ten processed), what holds its waiting
dumps (`held` by the threads hold, `waits_for` another run), and the
threads hold itself (`threads_hold`), with the spend today; it reads the
same from the files when the service is stopped (all but `waits_for`,
which only the running service knows). A dump's own listing
(`soup_dumps`, `soup_dump_get`, `cauldron soup dumps`) shows its state, and
once processed the run and the record ids. The dump receipt (`soup_dump`,
`soup_dump_session`, `cauldron soup dump`) says whether automatic
processing is on and takes that dump; `soup_dump` with `wait_secs` (and
`cauldron soup dump --wait-secs`), at most 120 seconds, waits for it and
answers the record ids.

**Turning it off.** `cauldron ripples off <cauldron> --env <env>` works
from anywhere, needs no terminal, and takes effect before the next model
call: a run in progress stops before its next call and before it would
write its records, and writes nothing more (an answer already received is
discarded and the dump stays `received`). Stopping the `ripples` service
stops it for every cauldron.

**Who can turn it on.** The terminal check guards the command, not the
file: anyone who can write the data root can write a setting file (and a
binding that matches the cauldron, which is computable from its
`cauldron.toml`), just as they can write `grants.json`. The setting keeps a
session or a script from turning it on through Cauldron; it is not a
defence against someone who can already write your files.

#### The cauldron's id

New in 0.1.12. `cauldron create` writes a random 128-bit id into the
cauldron's `cauldron.toml` (`id`). A rename keeps it, and it moves with the
cauldron's directory, so a data root moved whole keeps it too; no command
copies it to another cauldron. MCP grants and the
automatic ripples setting are bound to it: a cauldron made again under an
old name has another id, even when its `cauldron.toml` names the old
creation time, and is opened by neither.

A cauldron made before ids is given its id by the first writer that opens
it: the `mcp` service before a call reaches a granted cauldron, `cauldron
mcp grant`, `cauldron ripples on` or `cauldron rename`. At that moment,
and only then, its grant is carried onto the id if the grant records the
cauldron's creation time, and its automatic setting if the setting records
its creation time and its directory. Anything that does not match then is
refused, naming the command that makes it again (`cauldron mcp grant` or
`cauldron ripples on`); `cauldron mcp grants` and `cauldron mcp status`
name such a grant before its first refusal.

#### The secret scrubber

What a ripple sends to a model (always through threads; to a local model
unless `--no-scrub-local`) and every string of the model's answer before it
is written are passed through a scrubber that replaces each match with a
typed placeholder, `<redacted:kind>`, and counts it by kind in the run
journal (a `redacted` line; never the text). An answer's records are
written with the placeholders in them; a match does not refuse the batch.
It matches:

- private-key blocks (`-----BEGIN … PRIVATE KEY-----`, PGP private key
  blocks, four or five dashes, any case), whole or cut short;
- JSON Web Tokens;
- keys and tokens with well-known prefixes: Anthropic, OpenAI, AWS access
  key ids, Google API keys and OAuth tokens, GitHub, GitLab, Slack tokens
  and webhooks, Stripe secret keys, npm, PyPI, Hugging Face, SendGrid,
  DigitalOcean, Shopify;
- the password in a URL (`scheme://user:password@host`), a URL's userinfo
  that is one long token (`https://TOKEN@host`), and a secret in a URL's
  query (`?api_key=…`, `&token=…`);
- the value of an `Authorization:` or `Proxy-Authorization:` header, and of
  a `Cookie:` or `Set-Cookie:` header (the whole value, a token inside it
  included);
- a secret on a command line: the value of a long flag whose name holds a
  credential word as one of its hyphen-separated parts (`password`,
  `passwd`, `pwd`, `passphrase`, `secret`, `token`, `credential`, `apikey`,
  `api-key`, `private-key`, `access-key`, `auth-key`, `account-key`), as
  `--password x`, `--admin-password=x`, `--db-password "x"`, `--client-secret
  x`, `--access-token x` or `--storage-account-key x`, but not a flag that
  names something else after the word (`--api-key-id`, `--password-file`,
  `--password-stdin`, `--token-url`, `--secret-name`, `--token-ttl`, and the
  others the scrubber lists), nor a value that is a path; `-pVALUE` after
  `mysql` and its kin, a quoted `-p'…'` after any tool, `sshpass -p`,
  `redis-cli -a`;
- the value assigned to a name holding `SECRET`, `TOKEN`, `PASSWORD`,
  `PASSWD`, `PASSPHRASE`, `PWD`, `PASS` (as a whole part of the name),
  `CREDENTIAL`, `API_KEY`, `ACCESS_KEY`, `PRIVATE_KEY`, `AUTH_KEY` or
  `ACCOUNT_KEY`, in any case: `.env` style (`NAME=value`), lower case and
  dotted (`password=…`, `db.password=…`), with spaces (`api_key = …`, a
  value with a digit or a sign in it), quoted (`"password": "…"`, a
  passphrase of any length, a key inside it included), or YAML-ish
  (`password: …`, words to the end of the line, or one capitalised word);
- any other run of 32 or more base64 or URL-safe characters that mixes
  upper case, lower case and digits and whose character entropy is high,
  standing alone or as one segment of a path; except a digest in its
  obvious context, which is left: base64 of exactly a digest's length after
  a digest label starting a word, `sha1`, `sha224`, `sha256`, `sha384`,
  `sha512` (with or without a hyphen after `sha`) or `md5`, followed by `-`
  (`sha256-…`: Subresource Integrity, lockfiles, Nix), `=` or `=:`
  (`sha-256=…`: the HTTP `Digest` headers; also `sha256=…`, `md5=…`, and
  after a dot, `api.sha256=…`), a go.sum `h1:` hash, and the body of an
  image, font, audio or video `data:` URI. Git commit ids, UUIDs and hex digests are lower-case
  hex and never matched by this rule.

Matches that overlap or touch are replaced as one: their union, under the
outermost match's kind, with each kind inside it still counted. A match is
never dropped for overlapping another, so a token inside a `Cookie:` value
or a key inside a quoted passphrase never lets the rest of the value
through.

A value that reads as code or a stand-in is left alone: a variable or a
template (`$X`, `<…>`), a call or a type (`read_password()`, `u32`), a
dotted path (`self.token`), a number given to a name one of whose parts
counts (`TOKEN_LIMIT=4096`, `MAX_TOKENS=1000`, `tokenTimeout: 30000`; a
number of three or more digits given to `ADMIN_PASSWORD` or `PASSWORD` is
replaced).

**It reduces the risk; it is not a guarantee.** A secret in a format it does
not know, shorter than 32 characters and not given to a name or a flag it
knows, written in words outside an assignment, split across lines, made
only of hex digits, glued to the word before it, or written with look-alike
characters (a Unicode hyphen, a full-width letter) passes through, and so
does one written after a digest label with exactly that digest's length.
Its test corpus (`services/ripples/tests/it/scrub.rs`) generates samples
of every format above and misses none, and leaves a list of look-alikes
untouched (among them the flags that name something else), and touches
none of its generated digests, commit ids and UUIDs in their usual places
(it replaced 100 of those 180 samples before the digest rule); planted
beside one another and inside one another in every pairing, about 34,000
texts, it leaves no run of 8 characters of any of them (before matches
were merged, 359 of 26,593 such texts left part of a secret);
on this repository's own docs and code, which hold no secret, it replaced
nothing in about a million characters of prose and about one string per
million characters of code when measured (mostly string values given to
names like `token` in tests). It never changes the stored dump: removing a
dump is still the way to get a secret out of a cauldron.

**Its time is bounded.** Every pattern runs in time linear in the text (1
MiB of adversarial input takes well under a second), and a run gives it a
time in proportion to the text (2 seconds plus 20 per MiB). If it runs out,
nothing is sent (or, for the answer, written): the run fails
`scrub_timeout`, and automatic processing parks the dump.

### Local inference

New in 0.1.11. It has run against two real models (Qwen3.6-35B-A3B at Q8_0
and gpt-oss-120b at MXFP4, llama-server build 12110), but not yet through a
whole ripple.

The `inference` service runs a language model **on this machine**, with
llama.cpp's `llama-server`, for work where a smaller model is good enough
and nothing should leave the machine: summaries, curation, and turning a
dump into records (the dump ripple's `--backend local`). It is **not** for
writing code, settling a design question, or anything an agent acts on with tools:
it never offers a model tools, and nothing in cauldron gives it any.

**What it needs, and what it never does.** llama.cpp is not bundled:
install it yourself (on macOS, `brew install llama.cpp`), and the service
uses the `llama-server` on `PATH` (looked up again at every load), or the
one named by `llama_server = "/absolute/path"` in
`~/.cauldron/data/<env>/inference/config.toml`. Each time it loads a model
it first checks that binary with `--version` and `--help` (a build number
of at least 6000, and every flag it passes), and refuses a missing or older
one with what to do. That check proves the version and the flags, not where
the binary came from. Nothing is downloaded,
ever: you place a `.gguf` model file on this machine yourself, and the
model runs with `--offline`.

```sh
brew install llama.cpp                                              # yourself, once
cauldron --env prod build inference                                 # the service: off unless you start it
cauldron --env prod pin inference <the version label it printed>
cauldron --env prod start inference
cauldron inference models add qwen /Volumes/models/qwen.gguf --env prod   # record it: path, identity, sha256
cauldron inference models list --env prod                           # each with its memory estimate
cauldron inference status --env prod                                # loaded or not, memory, idle time, pid, crashes
cauldron ripples run DM-3 --env prod --cauldron kb --backend local  # a dump ripple, on this machine
```

**Registering a model copies nothing.** `cauldron inference models add
<name> <path> [--context <tokens>]` reads the file once for its sha256 (a
line every 4 GiB says how far it has got) and records, in
`data/<env>/inference/models.json` (owner-only), its path, its identity
(the real path with symlinks resolved, device, inode, size, and
modification and change times), its sha256 and a few facts from its
header. The header is read with every size it declares bounded, and a
header outside those bounds is refused, naming the field. Before every
load the file's identity is compared with the one recorded; if any of it
differs, the whole file is read again, and it is loaded only if its sha256
is the one registered, otherwise refused with how to register it again.
Reading a 60 GB file takes about two minutes, so that is not done at every
load unless `verify_every_load = true` is in the settings; `cauldron
inference models verify <name>` does it now. `models remove <name>` forgets
a model and leaves the file where it is.

**On demand, one model at a time.** The installer does not start the
service. Started, it loads nothing until a request names a model; then it
starts one `llama-server` child for it, on 127.0.0.1, on a port chosen at
each load. Its completions answer only a key made for that load
(`llama.key`, owner-only, removed when the child goes; any process of
yours can read it, as it can the service's door key); llama-server's
only `/health` answers without it, and says only that the child is up
(`/v1/models` needs the key too, in build 12110). A model nobody used for
15 minutes is unloaded and its memory is free again (`idle_minutes` in the
settings); `cauldron inference unload` does it now. Asking for another
model unloads the first. A child that crashes is recorded (`status` shows
its exit and the end of its log), and the next load waits 1 s, then 2, 4,
up to 60 s after repeated crashes. Stopping the service stops its child
first. If the service is killed, the guard process that holds the child
stops it within seconds; if the service and the guard are both killed, the
child runs until the service next starts, which stops it.

**The memory budget.** Before loading, the service estimates the memory a
model needs: the file's size, plus the KV cache for its context (layers ×
KV heads × (key + value width) × 2 bytes × context tokens, from the file's
header), plus 2 GiB for llama-server's buffers. Over the budget, 64 GiB
unless `memory_budget_gib` says otherwise, the model is refused, and the
refusal shows that arithmetic. The context is 65,536 tokens unless the
settings' `context` or the model's `--context` say otherwise; a smaller
context needs less memory, and a dump whose prompt does not fit is refused
by name. For example, gpt-oss-120b (a 59 GiB file) is refused at 65,536
tokens (65.53 GiB) and fits registered with `--context 32768` (63.28 GiB).
The estimate errs high: it counts every layer's cache in full, and
Qwen3.6-35B-A3B measured about 36 GiB resident against 41.37 GiB
estimated.

**A ripple, locally.** `cauldron ripples run <DM-n> --backend local [--model
<name>]` (the model defaults to the only one registered) sends the same
prompt and schema to the local model, and its answer is held to the same
schema, caps, journal and `unreviewed` marking as any ripple. Both of its
calls use the local model: there is no Opus retry, so nothing leaves the
machine. Local tokens cost no money, and the caps count them all the same:
3 calls per dump is what stops a runaway loop. The journal adds, per call,
the model, its file's sha256, the tokens and tokens per second. The
`ripples`, `mcp` and `inference` services must be running (not `threads`).

**Comparing backends.** `cauldron ripples eval --dumps <dir> --backends
threads,local --out <dir> --env <env> --cauldron <name> [--local-model
<name>] [--repeat <n>]` runs each fixture dump in `<dir>` (each `.txt` or
`.md` file is one dump) on each backend, and **writes nothing to any soup**:
every answer is judged by the `mcp` service's dry run and never landed. Into
the new `--out` directory go, per dump, each run's records and measures
side by side (`side-by-side.md`) and, for all of them, `summary.md`: schema
validity on the first try and after one retry, record count, records that
quote the dump, regions used, duplicate handles, tokens, time and tokens per
second. Which records are better is for you to judge from those files.
The threads runs are your Claude spend. With `threads` in `--backends`,
`--max-calls <n>` (at most 100) is required; the command prints the most
calls and the estimated input tokens first, and sends nothing unless you
add `--yes`. Each threads call is charged before it is sent, in the
evaluations' own ledger (`data/<env>/ripples/eval.jsonl`), with a daily
input-token cap of its own, and never against the cauldron's caps for runs.
A local-only evaluation needs neither flag. The evaluation runs in the
`ripples` service: Ctrl-C stops the command following it, and stopping the
service stops the evaluation. The `--out` directory is made owner-only.

## Known limitations

This is an early release. These are known:

- **macOS only.**
- **Nothing acts on a dump by itself unless you turn it on.** Curate a dump
  into records with `/cauldron:curate-dump`, run the dump ripple on it by
  hand (`cauldron ripples run`, see [Ripples, by hand](#ripples-by-hand)),
  or turn automatic processing on for a cauldron (`cauldron ripples on`,
  see [Ripples on their own](#ripples-on-their-own)). It is turned on for
  one cauldron, never for a name: removing, renaming or making a cauldron
  again under the name never carries it over.
- **The secret scrubber is not a guarantee.** It replaces known credential
  formats and long random strings in what a ripple sends and writes, and
  misses secrets in other shapes: do not run a ripple on a dump you know
  holds one (remove the dump instead). threads keeps the prompt of every
  ripple call (scrubbed) in its own store until you delete it. A dump
  longer than one call's input cap is not processed.
- **Automatic processing is tested with stand-ins only.** Its runs, kills,
  retries and caps are tested against a scripted backend, and its parks,
  fast failures and token counts against a fake threads service, with the
  real `mcp` door; no automatic run has been made against a real model or
  a real threads.
- **After a downgrade to 0.1.9, ripple records are not marked.** 0.1.9 reads
  a 0.1.10 store, but shows records a ripple wrote without `unreviewed`.
- **No prebuilt binaries.** `./scripts/install` builds everything from the
  checkout, which needs the Rust toolchain and takes a while.
- **A dump is removed from a shell only.** `cauldron soup dump-remove
  <DM-n> --env <env> --cauldron <cauldron> --yes` erases one dump's content
  from the cauldron's directory and leaves a tombstone; it is refused while
  the `mcp` service holds the dumps store (revoke or stop it first). The
  plugin's README has the details.
- **Claude Code is the only remote inference provider that runs.** Config
  shows the other token sources, endpoints and templates greyed out, and
  refuses to act on them. The one local provider is llama.cpp's
  `llama-server`, through the `inference` service.
- **Local inference has not run a whole ripple on a real model.** Real
  models have answered through the `inference` service, but `--backend
  local` and the evaluation have run only against a stand-in. A local run
  never falls back to Claude, and the evaluation reads fixture
  dumps from text files and does not measure recall or memory use. See
  [Local inference](#local-inference).
- **Parts of the UI read sample files, not the daemon.** These are the Status
  Mesh, Soup and Bus views and some Config views. The Threads tab, Status →
  Tokens and the threads face (`cauldron tui --chat`) are wired to the live
  threads daemon, and the Ledger reads the environment's own data. The
  Threads tab cannot start a new thread yet.
- **Cluster seeding runs a local stand-in.** The model-backed seeding call needs
  a `type-one` tool that this release does not include. Without it, cauldron
  seeds a new cluster by matching words between your boundary and the
  environment's contents, and labels the result `stand-in`.
- **The Ledger is drawn as a tree** rather than as fork-and-join rails.
- **Re-logging into a disabled Claude account does not re-enable it**, and the
  account pool can show a usage reading that is out of date. (A fix is
  planned.)
- **A large threads write-ahead log can slow the daemon's start.** A log left
  from earlier runs is folded only every five minutes, so a big one can stall
  startup. (A fix is planned.)

## Roadmap and changes

[ROADMAP.md](ROADMAP.md) lists what comes next, and [CHANGELOG.md](CHANGELOG.md)
lists what each release contains. Not baked yet, and on the roadmap: Codex
and other inference adapters, local models for more than the dump ripple
and the evaluation, encrypted cauldrons (fully
private, never leaving your machine), remote cauldrons, and meshes of them.

## License

Licensed under the [Apache License, Version 2.0](LICENSE). See [NOTICE](NOTICE).
