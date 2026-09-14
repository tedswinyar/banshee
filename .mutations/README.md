# .mutations — the durable mutation manifest

A pin is only real if the reverting mutation is applied and watched to fail
(CLAUDE.md's Testing standard). Historically that was done once, by hand, at
development time, and the result lived only in a commit message — so a refactor
three months later that moved the target text left the claim rotting silently:
nobody re-ran it, and nothing failed.

This directory makes the discipline **durable**. Each `*.mut` file is one
verified mutation — the exact production text, the mutation, and the test that
catches it. `scripts/replay-mutations.sh` (also `make mutations`) re-applies
every one through `scripts/mutate.sh` and fails if any mutation no longer
APPLIES (the text moved — `cargo fmt`, a refactor) or is no longer CAUGHT (the
pin rotted). It is deliberately NOT in the pre-push gate — each entry compiles
and runs a test, so the full replay takes minutes; run it before a release
(docs/release-checklist.md) and on the quarterly health check.

## Format (one mutation per file, `<slug>.mut`)

```
# free-text: what this pin proves
file: <repo-relative path>
test: <mutate.sh filter — a cargo/swift test name, or a scripts-suite test-* stem>
---OLD---
<exact current production text (copy it out of the CURRENT file; fmt moves it)>
---NEW---
<the mutation (may be empty to delete the OLD text)>
```

The header lines (`file:`/`test:`) and everything from `---OLD---` onward is
exactly what `scripts/mutate.sh <file> <test>` reads on stdin — replay just
splits the two.

## The convention

**When a commit claims a caught mutation, add it here.** The seed is this
session's cross-layer set (schema v8/v9, the reap-backup and daemon-traceability
work, the plist hardening); older mutations are backfilled as their files are
next touched, because an entry whose OLD text has already moved cannot be
reconstructed — only re-derived against current source.
