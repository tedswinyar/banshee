# Architecture Decision Records (ADRs)

Lightweight records of architectural decisions: the context, the decision, and
the consequences. Use [0000-template.md](0000-template.md) for new ADRs.

## When to write an ADR

Write an ADR when a decision meets ANY of these criteria:

- **Affects multiple components** — changes how layers interact or communicate
- **Hard to reverse** — switching later would be expensive (schema changes, protocol changes, fundamental architecture)
- **Was debated** — if the team spent >30 minutes discussing alternatives, capture why the winner won
- **Load-bearing simplicity** — a deliberate choice NOT to support something (e.g., "no multi-user sync") that people will re-propose

Do NOT write an ADR for:
- Technology choices that are easily swappable (logger library, test framework)
- Implementation details within a single module (unless they're novel/risky)
- Temporary workarounds (capture those in code comments with dates)

## ADR lifecycle and status values

### Status: accepted
The decision is implemented and in use. Most ADRs start and stay here.

**Example**: [0001-constellation-architecture.md](0001-constellation-architecture.md)

### Status: proposed
The decision is drafted but not yet implemented. Use this when documenting a
decision for future action.

**Example**: [0002-infra-as-dependency-deferred.md](0002-infra-as-dependency-deferred.md) (proposed + deferred)

### Status: superseded by ADR-NNNN
A newer ADR replaces this one. The old ADR remains for history; extract any
still-valid context into the new ADR.

**When to supersede**: When the fundamental approach changes but the problem
domain stays the same. Example: "ADR-0001: API as subprocess, superseded by
ADR-0005: API as embedded library" (hypothetical).

**How to supersede**:
1. Write the new ADR explaining the new decision
2. Update the old ADR's status to `superseded by ADR-NNNN`
3. Link both ways (old → new, new → "supersedes ADR-NNNN")

### Status: rejected
The decision was considered and explicitly rejected. These are valuable: they
prevent future-you from re-deriving the same idea and re-litigating it.

**When to reject**: When a proposal is debated and loses. Capture the
alternatives considered, why they lost, and what would have to change for the
decision to be reconsidered.

**Hypothetical example**:
```markdown
# ADR-NNNN: Sync the database between Macs via a cloud-synced folder (rejected)

## Context
A second Mac would be handy to watch from the first. Putting `banshee.db` in a
cloud-synced folder would carry it between them with no new code.

## Decision
Rejected. ADR-0001 rests on exactly one writer. A file synced by a third party
is a second writer nobody supervises, and SQLite in a syncing folder corrupts.

## Considered alternatives
- A read-only export shipped on a schedule (no second writer, but stale)
- The API over a tunnel (keeps one writer; punt until a second Mac exists)

## Rejection rationale
One writer is the load-bearing invariant. Any design that shares the file
gives it up, and the corruption shows up weeks later as a verdict nobody trusts.

## Reconsider if
- A second Mac actually needs watching from the first
- The transport can carry the API rather than the file
```

## ADR numbering

Sequential, zero-padded to 4 digits: `0000-template.md`, `0001-...`,
`0002-...`. Rejected ADRs still get a number — the sequence records the
chronology of decisions (and non-decisions).

## Cross-referencing

- ADRs may reference each other: "see ADR-0001 for the rationale"
- Link TO an ADR from the agent guide, the threat model, or other docs when the
  decision explains a constraint
- When superseding, link both ways

## See also

- [0000-template.md](0000-template.md) — copy this for new ADRs
- [0001-constellation-architecture.md](0001-constellation-architecture.md) — exemplar "accepted" ADR
- [0002-infra-as-dependency-deferred.md](0002-infra-as-dependency-deferred.md) — exemplar "proposed + deferred" ADR
