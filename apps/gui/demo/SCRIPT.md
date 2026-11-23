# Cauldron demo script: GotYaForce (v2)

Status: pre-written, awaiting approval. Nothing here has been built or recorded.

## Ground rules

**No model or agent is called, before or during the recording.** Claude wrote every agent
line in this file in advance, playing each role. Three labels are used throughout:

- **REAL**: executed by cauldron or git during the recording.
- **PLAYED**: text written in this file by Claude in the role named (ripple, coordinator,
  worker, verifier, coding agent). It is fed in at the exact point where a model's reply
  would arrive, so everything before and after that point is cauldron's own code.
- **SCRIPTED INPUT**: a click or typed line standing in for you.

The recording carries a permanent caption: "Agent output written in advance. No model called."

All project facts come from the GotYaForce repository as cloned on 2026-11-23: README.md,
docs/verification-status.md, docs/matching-loop.md, docs/acceleration-ninja-dash-motion.md.
No number below is invented; each appears in those files.

---

## Scene 0: Welcome (0:00)

- REAL: the app opens with no cauldron. Welcome shows the cauldron illustration and three
  choices: Clone from GitHub, Open a local folder, Open the demo cauldron.
- SCRIPTED INPUT: click "Clone from GitHub".

## Scene 1: Clone (0:06)

- SCRIPTED INPUT: type `abhidya/GotYaForce`. Destination and cauldron name are prefilled.
- REAL: git clones the repository. The progress bar shows git's phase text.
- REAL: the cauldron `gotyaforce` is created and linked to the clone.

Caption: "Start from a GitHub repo."

## Scene 2: Choose what it learns (0:18)

- REAL: the checklist lists documents found in the clone. Ticked: README.md,
  docs/verification-status.md, docs/matching-loop.md.
- SCRIPTED INPUT: click "Ingest 3".

Caption: "Docs only. You choose."

## Scene 3: Settings (0:26)

A short stop on the Settings window, opened from the title bar, so setup never needs a terminal.

| Section | Row | State shown | Label |
|---|---|---|---|
| Accounts | "No account signed in" with a Sign in button | Not signed in; a banner reads "Recording mode: agent output is pre-written" | REAL state |
| Agents | Claude Code may read and write this cauldron | On | REAL if the grant can be written on Linux (unverified); otherwise shown and labelled as display-only |
| Ripples | Run automatically on each dump | On | REAL (cauldron's per-cauldron setting) |
| Approval | Agents may approve records for me | Off | REAL |

- SCRIPTED INPUT: toggle "Run automatically on each dump" on; close Settings.

Caption: "Accounts, access and approval rules in one place."

## Scene 4: Drops, rings and the ripple (0:36)

- REAL: the three documents are stored as dumps DM-1, DM-2, DM-3.
- REAL: each dump is handed to the ripple.
- PLAYED (ripple): the reply for each dump, in the record format a ripple returns. Cauldron
  parses it and lands the records as unreviewed. REAL from that point.

**Ripple reply for DM-1 (README.md)**

| Id | Type | Handle | Body |
|---|---|---|---|
| DEC-1 | decision | The port route is matching decompilation. | Chosen 2026-09-04, superseding the wasm-unit pipeline. Candidate C is compiled and diffed against retail bytes until byte-identical. |
| CON-1 | constraint | Do not relaunch the wasm-unit driver. | `finish-port --drive` is obsolete on this route and must not be relaunched. Its queue, compile-fix loop and assembly gate are retired with it. |
| CON-2 | constraint | Match, synthetic test and playable parity are separate claims. | A recorded match, a passing synthetic test and a playable-parity result must never be reported as one another. |
| DAN-1 | danger | The matched-function count overstates progress. | 118 of the 405 matched functions are a single `blr`. True about the bytes, thin about the program. |
| IS-1 | issue | The compiler bounds the route. | mwcc-rs compiles only 10.07 % of the game's instructions; 87.2 % is compiler-blocked. |
| IS-2 | issue | The wasm-unit queue numbers cannot be reproduced. | They come from a machine-local, untracked state file and are not reproducible from a clone. |

**Ripple reply for DM-2 (verification-status.md)**

| Id | Type | Handle | Body |
|---|---|---|---|
| CON-3 | constraint | Only committed or re-derivable numbers are evidence. | Every number is read from a committed artifact or re-derived by a named script. A machine-local number says so and is not treated as evidence. |

**Ripple reply for DM-3 (matching-loop.md)**

| Id | Type | Handle | Body |
|---|---|---|---|
| CON-4 | constraint | A match resting on a masked data relocation is unverified. | Such verdicts are recorded `MATCH_UNVERIFIED` and are never written to `src-match/`. |
| T-1 | task | List the top 20 unmatched shape-shared classes and pick the next batch. | Use the matching loop's `shapes` command. No model step is needed for this class. |

- PLAYED (ripple): proposed clusters, each with a one-line boundary:
  1. **TypeScript recreation**: the hand-written three.js game that ships to GitHub Pages.
  2. **Matching decompilation**: compiling candidate C to byte-identical retail code.
  3. **Verification**: what counts as proven, and the words used for each claim.
  4. **Compiler**: mwcc-rs and what it refuses.
  5. **Retired: wasm-unit pipeline**: history only.
- PLAYED (ripple): links. CON-1 follows from DEC-1. DAN-1 qualifies DEC-1. IS-1 blocks DEC-1.
  CON-4 refines CON-2. IS-2 conflicts with CON-3.

On screen: three drops land; nine hollow dots appear on the first rings; link lines draw;
the arc between IS-2 and CON-3 turns amber.

Caption: "A ripple sorts each dump into records."

## Scene 5: The board (1:00)

Opened by clicking the amber arc. Posts 1 to 4 and 6 are PLAYED in the roles named.

1. **coordinator → @all**: Three documents in. @extract, pull decisions, constraints and dangers. @link, connect them. @verifier, check each record against the others.
2. **@extract → @link**: Nine records. Two are about the retired wasm-unit pipeline: CON-1 and IS-2.
3. **@link → @verifier**: CON-1 follows from DEC-1. DAN-1 qualifies the 405 figure. IS-1 blocks DEC-1. CON-4 refines CON-2.
4. **@verifier → @link, you**: IS-2 conflicts with CON-3. The queue numbers are machine-local, and CON-3 says machine-local numbers are not evidence. Keep IS-2 as an issue, or drop it?
5. **you → @verifier** (SCRIPTED INPUT): Keep it as an issue. Do not quote the numbers.
6. **@verifier → @all**: Kept. IS-2 is flagged "not evidence".

Caption: "Watch the work. Step in when it matters."

## Scene 6: Review (1:16)

- REAL: nine cards, one per record, each with a green "+" line and its cluster. IS-2 shows the verifier's flag.
- SCRIPTED INPUT: click "Approve all 9".
- REAL: the records are approved in the soup. The dots fill in and the surface settles.

Caption: "Nothing is written until you approve."

## Scene 7: A thread that starts informed (1:28)

The Threads view: a list on the left (one thread, "Clear the port queue"), the conversation on the right.

- SCRIPTED INPUT, typed as the prompt: "Relaunch the wasm-unit driver and clear the remaining queue."
- PLAYED (coding agent), first step: a tool call, `soup search "wasm-unit driver relaunch"`.
- REAL: the search runs against the soup. Its actual result is shown in the tool block. Expected: CON-1 and DEC-1.
- PLAYED (coding agent), reply:
  "I won't relaunch it. CON-1 says `finish-port --drive` is obsolete on the current route and
  must not be relaunched, and DEC-1 says the route is matching decompilation since 2026-09-04.
  The matching-route equivalent of clearing a queue is T-1: list the top 20 unmatched
  shape-shared classes and pick the next batch. I need your permission to run one command."

Caption: "The agent read the project's rules before acting."

## Scene 8: Cards written by the agent (1:44)

Both cards are PLAYED (coding agent) and stored as A2UI messages. The app draws them with one generic renderer; no screen was coded for either.

**Card 1, permission**
- Title: "Run the matching loop's shape listing?"
- Command strip: `python research/tools/matching-decomp/loop.py shapes --top 20`
- Note: "Read-only. Lists classes; changes no files."
- Buttons: Deny, Always allow in this cluster, Allow once.
- SCRIPTED INPUT: click "Allow once".
- PLAYED (coding agent): "Approved. This recording does not execute the command, so there is no output to report."
  **The command is not run. No result is shown or implied.**

**Card 2, visual check**
- Title: "Acceleration Ninja, action-2 dash: does the motion look right?"
- Left pane, "Browser build": a real frame if the game builds in the sandbox (unverified); otherwise a pane labelled "Capture placeholder".
- Right pane, "Reference": empty, labelled "No reference capture supplied".
- Text: "docs/acceleration-ninja-dash-motion.md says this change restores movement and does not establish parity."
- Options: Looks right, Too short, Too fast, Can't tell without a reference.
- SCRIPTED INPUT: click "Can't tell without a reference". No parity claim is recorded.

Caption: "Agents ask with cards, not paragraphs."

## Scene 9: Dump the session (2:02)

- SCRIPTED INPUT: click "Dump session" on the thread.
- REAL: the thread's transcript is stored as dump DM-4, kind "session".
- PLAYED (ripple), reply for DM-4:

| Id | Type | Handle | Body |
|---|---|---|---|
| DEC-2 | decision | The next batch comes from shape-shared classes, not the wasm queue. | Chosen in thread "Clear the port queue" after the request to relaunch the driver was declined under CON-1. |
| Q-1 | question | Does the Acceleration Ninja action-2 dash match the original? | Unanswered: no reference capture exists. Under CON-2 this is a playable-parity claim and needs one. |

- REAL: one new drop on the surface in **Matching decompilation**; two hollow dots; a line from Q-1 to CON-2.
- SCRIPTED INPUT: approve both.

Caption: "A finished session becomes knowledge."

## Scene 10: The next thread knows (2:18)

- SCRIPTED INPUT: new thread, prompt "What is still open on the ninja dash?"
- PLAYED (coding agent): tool call `soup search "ninja dash"`.
- REAL: the search runs; its actual result is shown. Expected: Q-1.
- PLAYED (coding agent): "One open question, Q-1: parity for the action-2 dash is unconfirmed because there is no reference capture. It was raised in the thread 'Clear the port queue'."

End card (2:30): "Real: clone, dumps, records, links, approvals, searches, settings. Written in advance: everything an agent says."

---

## How each gap is covered

| Gap | Covered by |
|---|---|
| Agent-authored UI | Scene 8: two cards written in the agent role, stored as A2UI, drawn by a generic renderer |
| Agents start informed | Scenes 7 and 10: the agent's first step is a soup search that really runs |
| A ripple sorting a dump | Scene 4: ripple replies fed in at the model boundary; parsing and landing are cauldron's code |
| Dumping a finished session | Scene 9 |
| Settings | Scene 3 |
| Threads view | Scenes 7 to 10 |

## What must be built

1. The desktop app shell, Welcome, Clone and the ingest checklist.
2. The Settings window.
3. The surface: drops, rings, cluster areas, links.
4. The board pane.
5. The review screen.
6. The Threads view with tool-call blocks and a "Dump session" button.
7. The A2UI card types and the generic renderer.
8. The task board (T-1 appears on it in scene 7).
9. A replay provider: the one substitution point, where a played reply is returned in place of a model's.

Already done: the UI reading the real soup (draft PR #2).

## Unverified, and what happens if each fails

| Assumption | If it fails |
|---|---|
| A Craft window renders in the sandbox | Stop and report. No silent switch to the terminal. |
| Cauldron's ripple and MCP services start on Linux without launchd | The replay provider lands records through the offline import path, and the script's labels for scenes 4, 6 and 9 change from "cauldron parses the reply" to "records imported"; approvals are then recorded by the app, not by cauldron's approve command. |
| The threads daemon can be made to compile on Linux | The Threads view reads the played transcript from a file, and scenes 7 to 10 are labelled as such. |
| GotYaForce's browser build runs in the sandbox | Card 2 shows the labelled placeholder. |
