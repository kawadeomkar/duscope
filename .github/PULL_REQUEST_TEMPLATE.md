<!--
Title: `<ID>: <package title>`, for example `P2-IDX-04: Bulk writer and atomic rename`.
One package per pull request. Replace every {placeholder}.
-->

**Package:** {ID}

## What was done

<!-- What changed and why, in a few sentences or bullets. -->

## Acceptance

<!--
Every acceptance check in the package row, with its result on each platform: the local run on
this machine, and a link to the CI run for the other. A check that cannot run in pull-request
CI (a nightly number, a self-hosted measurement, a manual check) gives the command, the machine
and the result file path.
-->

| Check | macOS | Linux |
| --- | --- | --- |
| {acceptance check} | {result} | {result} |

- [ ] `just check` is green
- [ ] every acceptance check is a test named `<id>_<behaviour>`
- [ ] a dependency change updates `DEPENDENCIES.md`
- [ ] a user-visible change updates the man page, and the schema fixtures if it touches the envelope or an MCP tool

## Not covered by the spec, and how it was decided

<!-- Each question the specification left open and the decision taken, or "Nothing." -->

## Deviation

<!--
Does this pull request deviate from the specification? Answer one of:
- No.
- Implements DEV-<n>, approved by the author (its commits are prefixed `DEV-<n>`).
- Blocked: this is a draft titled `<ID>: BLOCKED — proposed deviation` with the tests so far,
  and the proposed entry follows: finding, evidence, proposal rows revised, packages affected,
  proposed change.
-->

No.

## Spec changes

<!--
Where this work departs from plan/SPEC.md or plan/PROPOSAL.md, the edits to make once it
merges: the SPEC.md text as it should read, its Deviation log row (DEV-<n>, or a correction),
and the entry for PROPOSAL.md's changelog when the proposal changes. Or "None."
-->

None.

## Changelog

<!--
The `changelog.d/<ID>.md` fragment, pasted here; or "None: nothing is user-visible, so this
needs the `no-changelog` label", which the author sets.
-->

<!--
The author copies the last line into the Tracker's Notes column: keep it last. A deviation
draft says `Blocked` instead of `In review`.
-->
Tracker: {ID} In review — {one-line status}
