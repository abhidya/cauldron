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
  ripple turns one dump into records when you ask (`cauldron ripples run`);
  the engine that does it on its own arrives later.
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
  Ripples that act on their own come later; see [ROADMAP.md](ROADMAP.md).)

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

This is **0.1.8**. On top of the V1 interface (two tab bars, a first-run
screen, the Ledger, the account pool, secret human actions and `cauldron
ctl`, from 0.1.2 and 0.1.3), the 0.1.4 to 0.1.8 releases made a cauldron's
soup a knowledge graph you use from Claude Code:

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

0.1.10 adds the first ripple, run by hand: `cauldron ripples run` turns one
dump into unreviewed records ([Ripples, by hand](#ripples-by-hand)). Ripples
that act on dumps by themselves come later. [CHANGELOG.md](CHANGELOG.md)
lists every change.

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

**Not yet:** the engine that acts on a dump by itself. In the design, a dump
sets off **ripples**: agents that read it, update the soup, and turn it into
tasks, questions and actions. Nothing processes a dump on its own yet: you
curate one into the soup with `/cauldron:curate-dump`, or, from 0.1.10, run
the dump ripple on it by hand (`cauldron ripples run`). Questions
and actions appear in the UI, but they come from the environment's data or the
demo data. See
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
citing the dump it came from. In this release a ripple runs only when you run
it, on the one dump you name:

```sh
cauldron ripples run DM-3 --env prod --cauldron kb        # one dump; prints the records and the tokens
cauldron ripples status --env prod --cauldron kb          # the last runs, spend today and per dump, the caps
cauldron soup nodes --env prod --cauldron kb --review pending   # what a ripple wrote and nobody approved yet
cauldron soup approve C-12 IS-4 --env prod --cauldron kb  # you, at a terminal: mark records as reviewed
```

**Off unless you start it.** The work is done by the `ripples` service,
which the installer does not start. Start it yourself, in the environment:
`cauldron --env prod build ripples`, then `cauldron --env prod pin ripples
<the version label it printed>`, then `cauldron --env prod start ripples`.
The `mcp` and `threads` services must be running too (threads from this
release: an older threads is refused, see below), and the cauldron must be
granted to the MCP door with `read`, `write` and `dump` (the default
scopes). Even running, the service processes nothing on its own: only
`cauldron ripples run` sets it off. Stop it with `cauldron --env prod stop
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
  tool calls, tool output, thinking or shell output. Nothing removes secrets
  from the dump's text first. The schema is sent as the shape of the answer.
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
  counts, models, tokens and outcome, and never dump text or the prompt.
  The call's working directory is removed when the call ends.
- *Written to the soup*: the records, through the `mcp` service, audited in
  its `calls.jsonl` (counts and ids, never text).

So do not run a ripple on a dump that holds a secret: remove the dump
instead (`cauldron soup dump-remove`), before any run.

**Removing a dump after a ripple ran on it** erases the dump's content and
nothing else. Two things stay:

- *The records the ripple drew from it.* `dump-remove` does not touch them,
  and a model may have copied text from the dump into one. Find them by
  their cite: each carries `meta.cites: [{"kind": "dump", "id": "DM-3"}]`
  (`cauldron soup get <id>` shows it). The run's records are also listed by
  the dump itself before you remove it (`processed.records` in `cauldron
  soup dumps`), by `cauldron ripples status` (the run's `records`), and,
  while you have not approved them, by `cauldron soup nodes --review
  pending`. Supersede any record you do not want.
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

**Approving is yours, at a terminal.** `cauldron soup approve <id>... --env
<env> --cauldron <name>` gives each named record `meta.approved` (written
as the owner, in one write, audited in the `mcp` service's `calls.jsonl`),
and it reads as reviewed from then on; `--unapprove` takes it back. Like
`cauldron mcp grant`, it runs only with its input and output on an
interactive terminal, and no option skips that. No MCP tool approves, so no
Claude Code session can; a ripple never can (the soup refuses
`meta.approved` from a ripple). A review flow for sessions comes in a later
release. There is no reject command yet; a record you do not want stays
unreviewed, and can be superseded like any record.

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
Opus with the refusal; a busy backend is tried again, within the caps.
`run` prints each run's tokens (input, output, cached, the calls and the
models), and `status` shows the spend today per cauldron and per dump.

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

## Known limitations

This is an early release. These are known:

- **macOS only.**
- **Nothing acts on a dump by itself yet.** Curate a dump into records with
  `/cauldron:curate-dump`, or run the dump ripple on it by hand
  (`cauldron ripples run`, see [Ripples, by hand](#ripples-by-hand)).
- **A ripple sends a dump's text as it is.** Nothing removes secrets from
  it first: do not run a ripple on a dump that holds one (remove the dump
  instead). threads keeps the prompt of every ripple call in its own store
  until you delete it. A dump longer than one call's input cap is not
  processed.
- **After a downgrade to 0.1.9, ripple records are not marked.** 0.1.9 reads
  a 0.1.10 store, but shows records a ripple wrote without `unreviewed`.
- **No prebuilt binaries.** `./scripts/install` builds everything from the
  checkout, which needs the Rust toolchain and takes a while.
- **A dump is removed from a shell only.** `cauldron soup dump-remove
  <DM-n> --env <env> --cauldron <cauldron> --yes` erases one dump's content
  from the cauldron's directory and leaves a tombstone; it is refused while
  the `mcp` service holds the dumps store (revoke or stop it first). The
  plugin's README has the details.
- **Claude Code is the only inference provider that runs.** Config shows the
  other token sources, endpoints and templates greyed out, and refuses to act
  on them.
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
and other inference adapters, local models, encrypted cauldrons (fully
private, never leaving your machine), remote cauldrons, and meshes of them.

## License

Licensed under the [Apache License, Version 2.0](LICENSE). See [NOTICE](NOTICE).
