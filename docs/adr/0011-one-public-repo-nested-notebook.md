# ADR-0011: One public repository, with the private material in a nested notebook repo

- **Status**: accepted
- **Date**: 2026-09-13
- **Supersedes**: the two-repository design of 2026-09-03 (a private development
  repository mirrored, scrubbed, into a public one — recorded in the tracker, never as an
  ADR)

## Context

Banshee was developed in a private repository and published through a **scrubbed
mirror**: a script carried each private commit into a second, public repository,
stripping issue-tracker IDs, genericising maintainer prose, and refusing files marked
`export-ignore`. The design assumed the private tree was full of things the public
must not see and that the code itself needed a second copy to be safe.

A pre-publication review on 2026-09-13 — four independent audits of the public tree
(history, mechanism, identity, tone) — measured what was actually private. The answer
was **four things, none of them code**: the issue-tracker export (bead descriptions
are not public-grade prose), the session handoff, the build server's hostnames, and
the research notes. The agent guide, the mutation manifest and the
skills files measured zero sensitive hits and document the testing standard better
than anything else in the tree. Meanwhile the mirror had cost: every sync was a
three-way merge onto hand edits, and a scrub by shape had once rewritten crate names
(`banshee-api` matches the bead-ID pattern), with the damage reading as intent. The
private repository's history was never going to be published, so the public
repository would start from a fresh initial commit regardless.

## Decision

**We will keep ONE source repository, `tedswinyar/banshee`, and it will be public.**
The code, docs, ADRs, mutation manifest, skills, agent guides, workflows and
build-server runbook all live there, with every machine-specific value parameterised
out (`scripts/build-server/build-host.sh`) and the runbook saying `<build-host>`.

**The private material lives in `private/`, a separate git repository nested in the
checkout and gitignored by it**, pushed to a private repository. It holds the session handoff, the issue-tracker export
(`bd export --include-memories -o private/beads/issues.jsonl` — the tracker's only
durable copy, since `.beads/` is now wholly gitignored), the build host's details, the
residue denylist, the research notes, the template lineage, and verification
screenshots. `private/README.md` is its table of contents.

**The residue gate moves from the mirror into verify.** `scripts/lib/residue-gate.py`
runs in the scripts suite over every tracked file and refuses: a tracked notebook path
(`private/`, `HANDOFF.md`, `.beads/`, `docs/research/`, the template stamp, a release
identity file); a home path other than the documented `example` and `builder` users;
an RFC 1918 address; an e-mail address other than a GitHub noreply; and anything in
the optional private denylist. That last list lives in `private/`, because a list of
the names that must never appear is itself the leak. When the denylist is
absent (a fresh clone, the CI runner) the gate says so in its summary rather than
reporting a check it did not run. Bead IDs are no longer gated: the tracker is private,
but an ID is a harmless token, and stripping IDs by shape is the mistake the mirror
made.

**The public history starts fresh.** The cutover replaces the old mirror's history with
one initial commit of the prepared tree, force-pushed while the repository is still
private; after the flip the history is permanent. The old private repository is
archived, read-only, never republished.

**Releases publish to the repository's own Releases page**, so the workflow's own
token suffices and the separate releases repository and its cross-repo PAT retire.

## Consequences

Easier: one tree to edit, test and push; no merge of a scrubbed copy onto hand edits;
the agent guide and mutation manifest are public, which is what a reader auditing the
testing standard wants; releases need no second repository or secret; the build-server
runbook is public and reusable by a fork with two environment variables.

Harder: a gitignore is a promise, not a mechanism, so the gate must stay in verify and
GitHub's secret scanning + push protection stay on; the tracker's durability now
depends on `private/` being committed and pushed at session close (the close protocol
in `AGENTS.md` says so); the CI runner cannot apply the private denylist, only the
generic checks — a vendor name pasted into a doc is caught on the maintainer's machine
before push, not on the runner; and `ci.yml` must never regain a `pull_request`
trigger, because a fork's PR would run on the maintainer's machine as the build user.

Given up: the private repository's history as a live tree (it stays readable in the
archive); the ability to keep a "private draft" of a public file (there is one copy —
draft in `private/`, then move it).

Escape hatch: if the public tree ever needs a private variant of a file, the answer
is a `private/` copy and a public sentence, not a second repository. If the notebook
outgrows a directory, it is already its own repository and can move without touching
this one.
