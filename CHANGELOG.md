# Changelog

Every release of cauldron is listed here, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). The plan for upcoming releases is in
[ROADMAP.md](ROADMAP.md).

## [Unreleased]

Planned, and not released yet. Ripples and the soup engine arrive in 0.2.0.

### 0.2.0 (planned)

<!-- RIPPLES: filled when the ripple executor's RV1.S16 lands -->

- **Ripples live.** Dumps set off ripples, and ripples write to the soup. The
  soup engine is hardened, and the Ledger shows real ripple events. Neither is
  in any 0.1.x release so far.

### Not yet scheduled to a release

- **Fix: threads daemon boot with an oversized write-ahead log.** The daemon
  will fold the log once at boot, before recovery, and its periodic fold will
  first run at 30 seconds instead of 5 minutes. Through 0.1.7, a large log
  left from earlier runs can stall startup until the supervisor kills the daemon.
- **Fix: re-login re-enables a disabled account.** A verified login will lift
  the disable and trigger a fresh usage reading. Through 0.1.7, only an
  explicit enable clears it.

## [0.1.7] — 2026-11-16

Dumps: raw text put into a cauldron for later processing, from Claude Code
or a shell, including a whole Claude Code session's transcript. A dump is
kept in the cauldron's dumps store beside its soup, never in the soup, and
waits there in state `received`. Nothing processes dumps in this release;
processing of dumps comes in a later release. Stores made by 0.1.6 open
unchanged.

### Added

- **The dumps store.** Each cauldron's dumps live in `<cauldron>/dumps/`
  (for the implicit legacy cauldron, `data/<env>/soup/dumps/`), beside its
  soup: a spine of its own with one pool, `dumps`, whose grammar `dump@1`
  is grammar v7's `dump` node type copied (prefix `DM`, states `received`,
  `processed`, `failed`, the same fields) plus an optional `meta` object, and
  a content-addressed blob store, `blobs/<sha256>.<txt|jsonl>`. It is made on
  the first dump; a cauldron nobody has dumped into has none. Each dump is
  one transaction: its bytes are written to a temporary file in the blob
  store; then, holding the store, the grant is read again (by the service),
  the file is synced and named by its sha256, and one write creates
  `DM-<n>` in state `received`. A dump refused or failed at any step leaves
  no bytes: its temporary file is removed, and so is any blob no dump
  names. A writer killed mid-dump can leave a temporary file, or a blob and
  no node; both are removed the next time the store is opened for writing
  (the service's `open-dumps` audit line counts them, under
  `recovered_at_open.swept`). The
  same text dumped twice is two dumps and one blob. The store is private to
  its owner: its directories are made 0700 and its files 0600, whatever the
  umask. A soup record that
  came from a dump can cite it as `meta.cites: [{"kind": "dump", "id":
  "DM-3"}]` (documented in the schema's README; `meta` is open, so nothing
  in the grammar changed).
- **`soup_dump`** (MCP): keep up to 1 MiB of text as a dump, with an optional
  `title`, `source`, `cite` and `refs`. Larger text is refused as
  `too_large`. It answers a receipt: `dump_id`, `state: received`, `kind`,
  `bytes`, `sha256`, `blob_reused`, `tx`, the change `cursor`, and `next`
  (that processing comes later); never the text.
- **`soup_dump_session`** (MCP): keep a Claude Code session's transcript as
  a dump, verbatim, up to 64 MiB. The stdio bridge, which runs as the user
  beside the session, finds the transcript: the session is the call's
  `session_id`, else `$CLAUDE_CODE_SESSION_ID`; it must be a plain id (no
  path separator); and it is looked for only as
  `<config>/projects/*/<id>.jsonl`, `<config>` being `$CLAUDE_CONFIG_DIR`,
  else `~/.claude`. It follows no link there: a project directory or
  transcript that is a link, a transcript with a second name, or one whose
  real path is outside `<config>/projects` is `transcript_link`, and a FIFO,
  device or directory in the transcript's place is `transcript_unreadable`;
  nothing of it is read. No transcript of that id is `session_unknown`; a
  store or file that cannot be read is `transcript_unreadable`, not absent;
  over 64 MiB is `too_large`; each is answered before anything is sent. The
  bridge streams the transcript to the service's new route,
  `POST /mcp/dump-session` (the call on the first line, then the bytes); the
  service judges the call, grant included, before it reads any of the
  transcript, streams it to the blob store without holding it in memory,
  and never reads a path a call names. The receipt adds `session_id`,
  `lines` and `cwd`, and its `sha256` is the transcript file's. A
  transcript still being written is sent as it stood when the bridge looked
  at it, and when it has grown since, up to its last whole line. Called through
  `POST /mcp` instead, the tool is refused as `usage`.
- **`soup_dumps`** (MCP, `read`): the dumps, newest first, as summaries (id,
  state, kind, title, bytes, sha256, when received, source), never their
  text, filtered by `state` and `kind`. **`soup_dump_get`** (MCP, `read` and
  `dump`): one dump's content from a byte `offset`, as much as fits
  `max_chars`, with `next_offset` while there is more. Reading content needs
  both scopes because a dump is raw material: a transcript holds tool output
  and anything pasted into the session, and a grant made to read the
  knowledge graph does not hand it out to a Claude Code session. The scopes
  govern sessions through the MCP service only: `cauldron soup dumps` and
  `cauldron soup dump-get` run as the user and read dumps without a grant,
  like every other CLI read.
- **`cauldron soup dump <file|-> --env <env> --cauldron <name> [--title <t>]
  [--source <s>]`**, `cauldron soup dumps` and `cauldron soup dump-get <id>
  [--offset N] [--max-chars N] [--raw]` (`--raw` prints the content
  verbatim). `soup dump` writes directly when nothing holds the dumps store;
  while the `mcp` service holds it, the dump is sent to the service as one
  `soup_dump` and printed the same way, plus `forwarded_to`, as `soup
  import` does. The reads open the store so that they cannot write.
- **Plugin commands** `/cauldron:dump` (text, or a file's text) and
  `/cauldron:dump-session [title]` (this session's transcript, by
  `${CLAUDE_SESSION_ID}`), and README sections on dumps. A session
  transcript holds every message, every tool call and its output, and
  anything pasted into the session; it is stored verbatim, in the user's own
  cauldron on this machine.
- **A side-effect hook after each dump.** Once a dump's transaction has
  committed and the store has been let go, the service calls its
  `SideEffects` hook; in this release that only writes a log line. Later
  processing attaches there, after the write and never inside it.

### Changed

- **The `dump` scope is used.** A grant's `dump` scope (in the default
  `read,write,dump`) is what `soup_dump` and `soup_dump_session` need. The
  service opens a cauldron's dumps store on its first dump, under the
  store's own writer lock (`mcp <pid>`), and holds it until the grant loses
  `dump` or is revoked, or the service stops; it reads the grant again
  immediately before each dump is written, and every 4 MiB while a
  transcript arrives (a revoke stops the reading). A dump refused that way
  is answered "Nothing was written", and nothing is.
- **The change cursor has a fourth part**, the dumps store's counter:
  `v1.<instance>.<soup counter>.<dumps counter>`. A cursor 0.1.6 answered
  (three parts) is still accepted, read as dumps counter 0. A four-part
  cursor given to a 0.1.6 service is refused there as `cursor_unknown`.
- **`soup_changes` lists dumps** beside the soup's changes:
  `dumps: [{seq, at, op, id, kind, bytes, state}]`, ids and sizes only, cut
  by the same `limit` and `max_chars`, with the cursor moved back to the
  last item kept.
- **`soup_status` counts dumps:** `dumps: {store, total, by_state, held}`.
- **The audit log** gets one line per dump call (and per open and close of a
  dumps store), with the dump's id, its size and a transcript's line count,
  never its text.
- **`cauldron mcp status`** shows, per cauldron, what its dumps store's lock
  says (`dumps_lock_says`).

### Known limitations

- **A dump cannot be deleted.** No verb or tool removes one. To remove a
  cauldron's dumps (all of them, for example after a transcript holding a
  secret was dumped), stop the `mcp` service or revoke the cauldron, then
  remove `<root>/data/<env>/cauldrons/<cauldron>/dumps/` by hand
  (`<root>/data/<env>/soup/dumps/` for the implicit legacy cauldron). A
  blob alone is not enough: a text dump's text is also in the store's
  databases. The plugin's README has the steps.

### Compatibility

- A store made by 0.1.6 opens under 0.1.7 unchanged: nothing in its soup
  changes, and a cauldron has no dumps store until its first dump.
- A cauldron holding dumps works under the 0.1.6 binary: it reads and writes
  only `soup/` and does not look in `dumps/`. Checked with a 0.1.6 build
  (its `soup nodes`, `regions`, `search`, `get`, `import`, `recover`,
  `list` and `mcp status` on a cauldron with a dump), and by reading the
  0.1.6 service, which opens, reads and scans only the soup's directory.

## [0.1.6] — 2026-11-16

The MCP door: a Claude Code session can read, search and write the soups
their owner has opened to it, through a new supervised service. Every
cauldron is closed to it until the owner grants it at a terminal. Stores made
by 0.1.5 open unchanged.

### Added

- **The `mcp` service** (`services/mcp.toml`), one per environment, started
  like any other: `cauldron --env <env> start mcp`. It listens on
  `127.0.0.1` only (port 3654 in `prod`, a port the kernel chooses in every
  other environment, published through its seam) and answers MCP's JSON-RPC
  on `POST /mcp`. A request must carry the door secret, which the service
  mints at every start into `<root>/data/<env>/mcp/door.secret`
  (owner-only); a request with an `Origin` header, or with a `Host` that does
  not name the loopback door, is refused. These three are checked from the
  request's headers before any of its body is read. The door serves at most
  64 connections at once (the next waits to be accepted), reads only a body
  with a `Content-Length`, at most 16 MiB in one request (a larger one is
  refused as `too_large` from its `Content-Length`, unread) and 64 MiB of
  request bodies at once (a body's share is held until its answer is ready,
  and a request that cannot get its share within 10 seconds is answered
  `busy`), closes a connection that does not send a request's
  head within 5 seconds, and answers 408 to a body that does not arrive
  within 10.
- **Grants: every cauldron is closed until its owner opens it.** `cauldron
  mcp grant <cauldron> [--env <env>] [--scopes read,write,dump] [--project
  <dir>]...` runs only with its input and output on an interactive terminal,
  and no option skips that, so an agent cannot open a cauldron for itself.
  `cauldron mcp revoke <cauldron> [--project <dir>]` closes it again (or
  unbinds one project), and `cauldron mcp grants` lists them. The grants are
  kept in `<root>/data/<env>/mcp/grants.json` and checked on every call; no
  MCP tool can grant, revoke or list them. A grant opens the cauldron to the
  Claude Code sessions run by this user on this machine against that
  environment, whichever directory they work in. `--project` only tells a
  session which cauldron a directory means when it does not name one: a
  call's cauldron is its `cauldron` argument, else `$CAULDRON_CAULDRON`, else
  the grant whose project holds the session's working directory. The
  `.cauldron` marker never opens anything to the door. A grant is bound to
  the cauldron it was made for (its `created_at`, to the second), not only
  to its name: a cauldron removed and made again under the same name is
  refused as `cauldron_gone` until the owner grants it again. (A grant
  written by a pre-release build of 0.1.6 has no such binding and opens by
  name until it is granted again.)
- **The tools.** `soup_status` (which cauldron, how it was chosen, its
  grant's scopes, counts, the change cursor, and whether the service holds
  it open); `soup_regions`, `soup_region`, `soup_nodes`, `soup_search` and
  `soup_get`, which answer the same items as the `cauldron soup` reads, each
  reply's payload bounded as a whole by `max_chars` (default 12,000, floor
  256, ceiling 50,000; a value outside them is raised or lowered to the
  nearer) and saying what it left out; `soup_put`, records in the published
  shape judged and landed as one all-or-nothing write, with `dry_run`, `remint`,
  `partial`, `cite` and `allow_duplicate`, and the session cited on every
  record that has `meta` when the bridge knows the session's id; `soup_link`,
  edges between nodes the soup holds, all or nothing; `soup_changes`, what
  landed since a cursor (with no cursor, only the current one; a cursor of
  another cauldron, or of one removed and made again under the same name, is
  refused as `cursor_unknown`, and one ahead of the soup as `cursor_stale`).
  `soup_claim` is listed and always refused as `unsupported` (this schema
  has no clusters); `soup_dump` and `soup_dump_session` are listed and
  refused as `not_available_yet`. A refusal is a tool result with a typed
  code and a message that says what to do.
- **One writer per soup.** The service opens a granted cauldron for writing
  on its first write call, never at start, under the same proofs as an
  import, holds its soup's writer lock until the grant is revoked or loses
  `write` or the service stops, and runs every write on one thread per
  cauldron: concurrent sessions wait their turn and are never refused for
  it, except by the door's own limits above. Opening a soup an interrupted write left unsettled recovers it, and
  `soup_status` and `cauldron mcp status` say what that recovered
  (`recovered_at_open`). A revoke closes the cauldron and frees the lock
  even when no call follows it. The writer re-reads the grant immediately
  before it applies each write, so once a revoke (or the loss of `write`) is
  seen, every write still queued is refused as `not_granted` and nothing
  more lands; a write already being applied when the revoke comes finishes,
  all of it or none of it.
- **An audit line per write call** in `<root>/data/<env>/mcp/calls.jsonl`:
  when, which client and session, which cauldron and tool, the outcome and
  counts. Never record text.
- **`cauldron mcp stdio`**, the bridge Claude Code launches: JSON-RPC, one
  message per line, relayed to the service with the door secret, which it
  never prints. It adds the session's context to every tool call (its
  working directory, `$CLAUDE_CODE_SESSION_ID` when set, and `--cauldron` or
  `$CAULDRON_CAULDRON`) and replaces any context a caller put in the
  arguments. While the service is not running it still answers
  `initialize` and `tools/list`, and refuses every call as `service_down`
  with the command that starts it. Its environment is `--env`, else
  `$CAULDRON_ENV`, else `prod`; unlike the other verbs it does not read
  `$ENV`. When that environment cannot be used it still answers
  `initialize` and `tools/list`, and refuses every call as `no_environment`,
  saying why.
- **`cauldron mcp status`**: the service, the cauldrons it holds open and
  what opening each recovered, and per cauldron its grant and what its
  writer lock says.
- **The plugin's MCP server.** `claude-code/cauldron/.mcp.json` names
  `cauldron mcp stdio`. The curator and `/cauldron:curate` use the MCP
  tools first and the `cauldron` CLI as the fallback when the session has no
  MCP tools or the service is down; they never use the CLI to get round a
  refusal. The curator still states its target before its first write, and
  asks before writing to `prod` when the user did not name it.
- **`scripts/install` pins `mcp`** in an environment it creates, beside
  `threads`: a release build installed as the environment's own new version,
  then the pin. An existing `env.toml` is never edited; one without an `mcp`
  pin is told the two commands that add it. An install stopped between the
  two pins is finished by running it again.

### Changed

- **`cauldron soup import` while the service holds the soup.** A real import
  into a cauldron the `mcp` service holds is sent to the service and lands
  through it: the same judgement, the same JSON plus `forwarded_to`, and the
  same batch-already-landed check across both ways of landing, with
  `--allow-duplicate` and `--remint` carried. The service records the writer
  as `mcp:cli`, not `--agent`, and writes only the `ws` pool. A dry run is
  never sent. A batch larger than the 16 MiB the service reads in one request
  is refused before it is sent, naming the limit and the two ways on (smaller
  files, or revoking the grant or stopping the service and importing
  directly); it is never sent in parts, so an import still lands all of it or
  none of it. An ungranted cauldron, and every cauldron once the service has
  stopped, imports directly as before.
- **`cauldron soup recover` is refused while the service holds the soup,**
  naming it and its pid: the service recovers the soup itself when it opens
  it.
- **The soup logic is a library** (`libs/soup-door`), shared by the CLI and
  the service. A recorded session of every `cauldron soup` verb (imports,
  reads, refusals and help) prints the same bytes as 0.1.5 did, apart from
  the help text, which now describes the two changes above.

## [0.1.5] — 2026-11-16

Small fixes to the soup commands, and a way for a project directory to name
its cauldron. Nothing new runs in the background, and stores made by 0.1.4
open unchanged.

### Added

- **`cauldron link <cauldron> --env <env>`.** Writes a `.cauldron` marker
  file into a project directory, after checking that the environment and the
  cauldron exist. `cauldron soup` run in that directory or below it then
  needs neither `--env` nor `--cauldron`. An explicit flag or variable always
  wins over the marker (a variable that names no usable environment is an
  error, not a gap the marker fills), and an explicit environment that
  differs from the marker's, with no cauldron named, is refused rather than
  guessed. Given both `--env` and `--cauldron`, a verb reads no marker and
  not even the current directory. `cauldron link --show` prints what a
  directory resolves to and where each value came from. Without a marker the
  verbs behave as before, and nothing defaults to `prod`. The plugin's
  curator and `/cauldron:curate` use it when no cauldron is named.
- **The marker is per-machine, and a shared one cannot aim a write.** It
  names an environment and a cauldron on this machine, never a path or a
  pool, so keep it out of version control: add `.cauldron` to `.gitignore`.
  `cauldron link` prints a hint when it writes a marker in a git work tree
  that does not ignore it, and never edits `.gitignore` itself. A marker in
  any parent directory is obeyed, including one that came with a cloned
  repository, so every soup verb that took its target from a marker says so
  on stderr and as `"resolved_from": "marker:<path>"` in its JSON. A write
  (`cauldron soup import` without `--dry-run`, and `recover`) refuses to take
  its target from a marker that git tracks or that another user owns;
  `--env` and `--cauldron` given explicitly still write, and reads are not
  affected. `cauldron link --show` reports that judgement as
  `writes_refused`. A `.cauldron` that is a symbolic link is never followed
  when read, and `cauldron link` never writes through one, even with
  `--replace`. The plugin's curator states a target it took from a marker
  before its first write, and asks before writing to `prod` from one.
- **`cauldron soup import` refuses a batch that already landed.** Re-importing
  the same records (as read, before `--remint`) into the same pool is refused
  as `batch-already-landed`, with when that batch landed and how many records
  it held. This includes an import that `cauldron soup recover` finished.
  Before, with `--remint`, the second import landed every record again under
  new ids. `--allow-duplicate` lands a second copy on purpose. Each import
  records its batch's digest in its own transaction row, so batches landed by
  0.1.4 or earlier are not recognised.

### Fixed

- **`--pool` must be a pool name.** It was joined into a file path unchecked,
  so a path or `..` could open a file outside the soup (read-only). It is
  now refused unless it is `[a-z][a-z0-9-]*` and not `spine`.
- **`cauldron soup help` lists the refusals this release makes.** It said
  `import` and `recover` were refused while a soup daemon runs, but this
  release has no soup daemon. It now names the writer lock, a soup not at
  rest, and the environment's seam table.
- **Five error messages no longer carry long runs of spaces** in the middle of
  a sentence: the tranche timeout check, the newer-spine refusal, two
  `cauldron install` messages and the `--addr` refusal.

## [0.1.4] — 2026-11-16

A cauldron's soup as a knowledge graph you can use from Claude Code. Nothing
new runs in the background: there is still no soup engine and no ripples, no
new service or daemon, and nothing changes for an existing install until you
run one of the new commands. The soup here is a store you write and read
with the `cauldron` command.

### Added

- **The soup's schema is published: `schema/soup/workspace-v6/`.** The soup's
  grammar as JSON (`grammar.json`), one JSON Schema per node type, the edge
  table (`edges.json`), and a README for anyone writing records from outside.
  It is generated from the soup's grammar, and a test fails if the two drift.
- **`cauldron create <cauldron> --env <env>`** makes a cauldron, with its own
  empty soup, under `data/<env>/cauldrons/<cauldron>/`, and `cauldron list
  --env <env>` lists an environment's cauldrons. (`--seed` is not in this
  release and is refused.)
- **`cauldron soup import | regions | get | nodes | search | region`.** Land
  records written in the published shape into a cauldron's soup, as one
  all-or-nothing write after each record is checked against the soup's
  grammar (`--dry-run` checks without writing; `--remint` gives the records
  fresh ids). Then read them back: every region, one region with its
  neighbours and members, nodes newest first, a word search over handles and
  bodies, and whole records by id. Every command names its environment, prints
  JSON, and exits non-zero on a refusal; the reads never write. An import that
  is interrupted lands nothing until `cauldron soup recover` settles the soup:
  recover either finishes that import (it says `landed`, with the ids; do not
  import that batch again) or rolls it back (it says `rolled-back`; importing
  it again is safe), and the next import can then run.
- **A Claude Code plugin: `claude-code/cauldron/`.** A curator subagent and a
  `/cauldron:curate` command that turn notes and findings into soup records
  and land them with `cauldron soup import`, plus a copy of the schema.
  `claude-code/cauldron/README.md` says how to install it and walks through a
  session.

### Changed

- **`cauldron env create` accepts `--no-engine`.** This release has no engine,
  so it changes nothing, and the command says so.

## [0.1.3] — 2026-10-31

Account polish. Ripples and the soup engine arrive in 0.2.0: nothing in this
release acts on a dump yet.

### Changed

- **Account settings fail safe when the install's key is wrong.** Account ids
  are salted with a key kept per install (0.1.2). If that key belongs to
  another install, cannot be read, is not a key, or has gone missing after one
  was made, the face no longer mints a new key or adopts another install's:
  it opens account settings read-only and says why in a notice. Changes made
  in that face are not saved. Where it is true, the notice also says that
  settings saved for an email-address account are not applied in that
  session; a named account's settings still apply.
- **Clearer refusals.** When account settings are read-only because of the
  install's key, adding or changing an account or an endpoint is refused with
  a message that names the key problem, instead of blaming `--dump`.
- **A face no longer waits forever on a service that does not answer.** The
  face's connection to an environment's services gives up after 5 seconds
  when the socket accepts but never completes its handshake, marks the link
  down with the reason, and retries.

### Fixed

- **Scheduled jobs fire at the right local time across daylight-saving
  changes.** A job's daily slot is worked out as a local date and time, not
  as "24 hours ago", so a change of clocks no longer moves it by an hour.
  When the time happens twice (clocks go back), the job runs at the first
  occurrence; when it does not exist (clocks go forward), it runs that much
  later (02:30 runs at 03:30).
- **Uptime is correct across daylight-saving changes.** A service's uptime is
  measured between two instants instead of two local clock readings, so it no
  longer reads an hour short or long after the clocks change.
- **`cauldron pin` says the right thing when a park lands at the same
  moment.** Pin reads the service's state and saves the pin as one step, so
  its message no longer says a service "is not running" when the supervisor
  revived it while the pin was being saved.

## [0.1.2] — 2026-10-31

The V1 interface. Ripples and the soup engine arrive in 0.2.0: nothing in
this release acts on a dump yet.

### Added

- **Two tab bars.** A global bar on top, Config, holds TUI, Devices, Providers
  and Accounts. Under it, the environment bar holds Cauldron, Threads, Status,
  Permissions and Settings (this environment's own settings, which Config
  called `<name> cauldron` in 0.1.x), with the environment dropdown at its
  left. The two bars are drawn in different colors, and one tab is shown at a
  time. The single Config tab of 0.1.x is gone.
- **First run.** An empty install opens on one action, "+ create your first
  cauldron", and makes nothing until you take it. A cauldron with no cluster
  of its own opens with the sidebar's `+` selected, ready to name its first
  cluster. When no cauldron is named, the face and `cauldron tui` open the
  last-used one.
- **Secret human actions.** A secret field of a human action is typed into a
  masked input, and its value is routed to the environment's env file or the
  macOS keychain, never into the soup. A non-secret field can also be routed
  to the action's graph node. A secret field with no destination, two fields
  with one name, or two fields routed to one env variable is rejected when
  the action is written, and the face runs the same check before it draws one.
- **`cauldron ctl`: the face, headless.** `ctl dump --env <name>` sends one
  dump (its text on stdin) through the TUI's own send — the same line in
  `dumps.jsonl` the send key writes; `ctl frame` prints one frame of the face
  (`ctl frame --status-tab ledger`); `ctl node <id>` reads one node from the
  environment's soup door at the address its seam table declares. JSON out,
  non-zero exit on any refusal; `dump` is the only write, and the
  `cauldron-build` demo refuses it. The face gains `--dump --send`, which
  `ctl dump` runs. A `ctl dump` sends only its own text: the draft open in
  your face is never read into it.
- **Config → Accounts.** Accounts are global and come from the threads
  daemon: the roster is the daemon's own account list, and when it is not
  there yet the page says why (not connected, connecting, threads is down,
  loading). Log in once, from the face: "+ add account", or an account's
  "log in", opens a login window where you paste the code (drawn as dots).
  Each account can be taken out of the pool or put back, and switched on or
  off for new cauldrons; each cauldron's Settings → Accounts can inherit that
  default or turn the account on or off for itself. Addresses are masked
  everywhere as `a•••@e•••`, and two addresses that mask alike get a short
  tag to tell them apart (`c•••@e••• ·d64`). Account ids are salted per
  install, and settings saved under the old ids carry over. A login code is
  never printed in a debug line, and the face's log keeps raw daemon frames
  out at every log level.

### Changed

- **Status → Ledger, first version.** The Ledger shows dumps as a time-ordered
  event flow, newest first; each dump expands into its phases and the tree of
  work under it. It reads the environment's own data: a real environment,
  `prod` included, shows its own ledger or an empty one, never sample data.
  The `cauldron-build` demo environment carries a demo ledger.
- **Status → Tokens reads the real account pool.** Claude Code's band shows
  the threads daemon's accounts, their usage meters and their placement
  pressure, over the face's one connection to the daemon, instead of sample
  data. Values not read yet show as loading, never as missing.
- **Inference: Claude Code is the only live inference source.** Codex, the
  other token sources, every endpoint and every template are shown greyed out,
  never hidden, and acting on one is refused with a notice. What you already
  wrote for an endpoint is kept.
- **Cleanup.** The ripple kind once called "splash" is now "type 3" (stored as
  `type_three`; policies that say `shell` still read). Threads no longer show
  an "absent" status: a value that is not there is left blank, and one not
  read yet shows `…`.
- **Threads tab, honest when the daemon never answered.** A Threads tab whose
  daemon was dialled but never reached now reads "threads is down · it has not
  answered yet" instead of claiming to show what it last said.
- **Isolation: every never-dial port is refused.** An isolated face (see
  0.1.1) refuses an environment that declares threads on any port the egress
  check never dials, and says so in its own words.

## [0.1.1] — 2026-10-31

### Added

- **The Threads tab.** The environment's tab bar now reads Cauldron, Threads,
  Status, Config; Threads replaces the Agents tab, which drew sample data.
  It shows the environment's threads pinned or recent, plus closed threads
  under Archived, read live from that environment's threads daemon and built
  on the threads face's client (the same connection and store as
  `cauldron tui --chat`). Open a thread to read its transcript, send it a
  message, open a tool call to see its full arguments and result, and see a
  Task subagent's own transcript. When the daemon is down, the last rows stay
  drawn and sending is refused with "can't send — threads is down". The tab
  connects to the daemon the first time it is shown. Switching cauldron keeps
  the Threads tab connected to the new environment's own threads endpoint.
  Starting a new thread from this tab is not in this release (the threads
  face has it).

### Changed

- **Isolation hardening for tests.** A test, or any face or CLI run with
  `CAULDRON_ISOLATED=1`, can no longer reach a real installation. Every dial
  and every daemon-control spawn goes through one egress check
  (`libs/env/src/egress.rs`): a dial is admitted only to a loopback listener
  that the isolated process tree itself bound, paid or third-party APIs are
  refused, starting or stopping a daemon is refused unless it targets a
  scratch environment, and a root inside the real `~/.cauldron` or the login
  home is refused. A non-interactive face needs an explicit endpoint, and
  `cauldron-launcher` forwards only from a scratch root to a scratch binary.
  The test binaries set the marker themselves (`testkit/test-isolation`).
  Not isolated, nothing changes.

## [0.1.0] — 2026-10-31 — MVP (Halloween)

The first public release.

### Added

- **The new terminal UI.** It shows environments, the cluster sidebar with
  clusters seeded from a boundary you write, cluster tabs (Questions, Actions,
  Tasks, Nodes, Status, Details), the bucket and dumps, and the Ledger.
  `cauldron tui --chat` opens the threads face.
- **The release branch:** the main line merged with the new UI.
- **`cauldron-build` demo environment.** Curated data from cauldron's own
  development, scrubbed of names, paths and secrets, shipped in the soup crate.
  It shows clusters and the aggregate cluster, directives, constraints,
  objectives and milestones, questions and multi-phase actions (including a
  secret action), dumps, the Ledger, and opening a file.
- **A fresh environment starts empty;** the sample data lives in the
  `cauldron-build` demo environment.
- **Install path.** `scripts/install` builds and installs cauldron for the
  current user under their own home directory. It refuses anything but macOS,
  uses ad-hoc code signing by default (no personal signing identity), uses
  neutral launchd labels, and offers `--dry-run`. Both `scripts/install` and
  `cauldron install` refuse to restart a cauldron job that is already running
  unless you pass `--replace-running`.
- **License and docs.** Apache-2.0 `LICENSE` and `NOTICE`, the README with its
  image, this changelog and `ROADMAP.md`.

### Removed

- **Internal development notes and personal data** are kept out of the public
  tree: step reports, runbooks, design notes and crate-provenance notes are not
  shipped, nor are the proof scripts that run against one machine's live
  services. Shipped files name no home directory of a real user and no private
  network address; the only person named is the developer, in `NOTICE` and the
  README credit. Source comments still carry provenance tags (step ids,
  `msg N`, `INTENT N`), short quoted product direction from development
  sessions, and the names of the private repositories and workspace paths
  cauldron was built alongside; the live-store guard in `libs/env` names, by
  design, the directories it refuses to write. Two checks fail if a listed
  path or a personal value comes back: `scripts/check-public-tree.sh` (which
  paths ship) and `scripts/check-no-personal-data.sh` (what the shipped files
  say).

[Unreleased]: https://github.com/broomstick-ai/cauldron/compare/v0.1.7...HEAD
[0.1.7]: https://github.com/broomstick-ai/cauldron/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/broomstick-ai/cauldron/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/broomstick-ai/cauldron/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/broomstick-ai/cauldron/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/broomstick-ai/cauldron/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/broomstick-ai/cauldron/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/broomstick-ai/cauldron/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/broomstick-ai/cauldron/releases/tag/v0.1.0
