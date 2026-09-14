# Skills — Agent Guidance

Language and layer-specific conventions for agents working in this codebase.
Each skill targets a specific development context (Swift views, API patterns,
etc.) and provides rules, anti-patterns, and references.

## Current skills

- **[swift-conventions.md](swift-conventions.md)** — SwiftUI view layer: design tokens, no raw spacing/colors, preview macros
- **[api-patterns.md](api-patterns.md)** — Rust API layer: deny_unknown_fields, ApiError mapping, store patterns

## Skill format (standard)

Use this structure for consistency across skills:

```markdown
# [Area] Conventions

**Context**: One line: what agent/layer is this for? When to read it?

## Rules

**MUST**:
- (Hard requirements; breaking these is a review finding)
- Example: All SwiftUI views MUST use design tokens, never raw values

**SHOULD**:
- (Strong recommendations; exceptions need rationale in PR)
- Example: Extract repeated logic into a helper rather than copy-paste

**MAY**:
- (Optional patterns; choose based on context)
- Example: Consider using a @ViewBuilder for complex conditional views

## Common mistakes

For each anti-pattern, show the wrong way and the right way with brief "why":

- ❌ **Don't**: `Text("Hello").padding(16)` — hardcoded spacing
  ✅ **Instead**: `Text("Hello").padding(Spacing.md)` — design token
  **Why**: Raw values drift; tokens stay consistent and are theme-aware

## References

- Link to CLAUDE.md sections or ADRs that provide deeper context
- Example: See CLAUDE.md → Wire format for the Five Rules
```

## When to add a skill

Add a new skill file when:
- A pattern is repeated across 3+ files and isn't obvious
- An agent made the same mistake twice in different sessions
- A review finding is likely to recur (bad default, non-obvious convention)

Do NOT add skills for:
- One-off fixes (put those in comments or CLAUDE.md Gotchas)
- Language basics (agents know Rust/Swift syntax)
- Anything already well-covered in CLAUDE.md or the ADRs

## Skill maintenance

- Update skills when conventions change (e.g., new design token structure)
- Add to "Common mistakes" when a review finds a new anti-pattern
- Keep skills under 30 lines when possible — link to CLAUDE.md for depth
- Remove stale skills when the layer is pruned (e.g., delete swift-conventions.md if --no-swift)
