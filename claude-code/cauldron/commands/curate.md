---
description: Record notes or findings into a Cauldron soup through the curator subagent
argument-hint: <cauldron> [--env <env>] [what to record]
---

Dispatch the `cauldron:curator` subagent to record material into a Cauldron soup.

Arguments: $ARGUMENTS

The first argument is the cauldron's name. `--env <env>` names the environment;
if it is not given, ask the user which environment to use (the `cauldron` CLI
never defaults to production, and neither should you). Anything after that
describes what to record. If nothing is described, use the substance of this
conversation that is worth keeping: decisions made, rules learned, defects
found, open questions, with the files, commits or links that support them.

Hand the curator, in its prompt: the cauldron name, the environment, and the
material itself written out in full (it cannot see this conversation). It
finds what the soup already holds with `cauldron soup search`, `nodes` and
`region`, so it needs no ids from you. If the
user quoted their own words as direction and gave a citation for them, pass
the citation along; otherwise say there is none.

When it returns, relay its report: the new ids with their handles and regions,
anything it left out, and any region it proposes. A proposed region is never
created without the user's approval; if they approve it, dispatch the curator
again with the approved region and the records it could not place.
