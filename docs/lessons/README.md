# Lessons

Evidence-backed write-ups of things this project learned the hard way. Not a
diary — each lesson exists because the same class of mistake was about to be
made twice.

## The bar for a lesson

1. **It cost something real** (a broken release, silent data loss, hours of
   misdiagnosis) — cite what.
2. **It is checkable** — show the commands/output that demonstrate the
   failure and the fix, so a reader can verify rather than believe.
3. **It changes behavior** — end with the rule that now applies, and wire
   that rule into a gate (verify suite, hook, review checklist) where
   possible. A lesson that is only prose will be relearned.

## Format

`NN-short-slug.md`, numbered in discovery order:

```markdown
# NN. Title stating the rule, not the story

**Cost**: what it broke and how long it took to find.
**Evidence**: commands, output, commit hashes.
**Rule**: the one-sentence behavior change.
**Enforcement**: which gate/test now catches it (or why none can).
```

When a lesson earns a one-liner in `docs/troubleshooting.md`, link back to the
full write-up here.
