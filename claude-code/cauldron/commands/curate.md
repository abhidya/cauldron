---
description: Record notes or findings into a Cauldron soup through the curator subagent
argument-hint: [<cauldron>] [--env <env>] [what to record]
---

Dispatch the `cauldron:curator` subagent to record material into a Cauldron soup.

Arguments: $ARGUMENTS

The first argument is the cauldron's name, and `--env <env>` names the
environment. If either is missing, run `cauldron link --show` in the working
directory: if the project is linked (a `.cauldron` marker, written by
`cauldron link`), it prints the environment and cauldron to use, and an
explicit argument still wins over it. If it finds no marker, ask the user
which cauldron and environment to use (the `cauldron` CLI never defaults to
production, and neither should you). When the target came from a marker and
not from the user's words, tell the user the environment, the cauldron and
the marker's path before dispatching (a marker can arrive with a cloned
repository); if that environment is `prod`, ask the user to confirm writing
there first, and dispatch only once they have. If `link --show` reports a
non-null `writes_refused`, show the user that text and ask them to name the
target instead. When the first word could be either a
cauldron name or the start of the material, check it against
`cauldron list --env <env>`. Anything after the target describes what to
record. If nothing is described, use the substance of this
conversation that is worth keeping: decisions made, rules learned, defects
found, open questions, with the files, commits or links that support them.

Hand the curator, in its prompt: the cauldron name and the environment (as
resolved, and from where, with the marker's path if a marker supplied them,
and whether the user confirmed a `prod` target), and the
material itself written out in full (it cannot see this conversation). It
finds what the soup already holds with `cauldron soup search`, `nodes` and
`region`, so it needs no ids from you. If the
user quoted their own words as direction and gave a citation for them, pass
the citation along; otherwise say there is none.

When it returns, relay its report: the new ids with their handles and regions,
anything it left out, and any region it proposes. A proposed region is never
created without the user's approval; if they approve it, dispatch the curator
again with the approved region and the records it could not place.
