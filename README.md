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
  a working session](#from-install-to-a-working-session)). The engine that
  turns dumps into soup on its own (ripples) arrives in 0.2.0.
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
  curate the soup and complete your tasks.** (Not in this build yet. Ripples
  are coming in 0.2.0; see [ROADMAP.md](ROADMAP.md).)

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
directories are mode 0700 and its files 0600).

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

Ripples, the engine that acts on dumps by itself, arrive in 0.2.0.
[CHANGELOG.md](CHANGELOG.md) lists every change.

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
tasks, questions and actions. Ripples are not in this build yet, so nothing
processes a dump on its own (you can curate one into the soup with
`/cauldron:curate-dump`); they are coming in 0.2.0. Questions
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

## Known limitations

This is an early release. These are known:

- **macOS only.**
- **Nothing acts on a dump by itself yet.** Ripples are coming in 0.2.0 (see
  above); until then, curate a dump into records with
  `/cauldron:curate-dump`.
- **No prebuilt binaries.** `./scripts/install` builds everything from the
  checkout, which needs the Rust toolchain and takes a while.
- **A dump cannot be deleted yet.** No verb removes one. To remove a
  cauldron's dumps (all of them), stop the `mcp` service or revoke the
  cauldron, then remove `<root>/data/<env>/cauldrons/<cauldron>/dumps/` by
  hand; the plugin's README has the steps.
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
