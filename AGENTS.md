# Banshee — Agent Operating Notes

## Issue Tracking

This project uses **bd (beads)** for issue tracking. Run `bd prime` for
workflow context.

> **PROJECT DEVIATIONS — record them HERE the moment you hit one.**
> Stock beads instructions go stale against the installed bd version and this
> repo's configuration; a deviation that lives only in one agent's memory gets
> re-discovered (expensively) by the next. Known deviations:
>
> **1. `bd hooks install` must ONLY run via the temporary-hooksPath dance
> below.** Run bare, bd points `core.hooksPath` at `.beads/hooks`, which shadows
> every hook already installed in `.git/hooks` and turns a directory bd
> auto-commits into a hook directory that other tooling on the machine may write
> into (pushes then fail with `exec format error`, and whatever landed there is
> committed). The correct sequence:
>
> ```bash
> rm -f .beads/hooks/*
> git config --local core.hooksPath .git/hooks
> bd hooks install
> git config --local --unset core.hooksPath
> ./scripts/install-hooks.sh   # re-layers the verify gate; preserves the beads block
> ./scripts/check-hooks.sh     # must say "baseline OK"
> ```
>
> `.beads/hooks/` is gitignored and must stay empty.
>
> _(add the next deviation here, with the date and what it cost)_

**Quick reference:**

```bash
bd ready --json                                  # unblocked work
bd create "Title" -d "Context" -t task -p 2 --json
bd create "Found bug" -d "Details" -p 1 --deps discovered-from:<parent-id> --json
bd update <id> --claim --json                    # claim atomically
bd close <id> --reason "Done" --json
```

### Issue Types

- `bug` — something broken
- `feature` — new functionality
- `task` — work item (tests, docs, refactoring)
- `epic` — large feature with subtasks
- `chore` — maintenance (dependencies, tooling)

### Priorities

- `0` critical (security, data loss, broken builds)
- `1` high · `2` medium (default) · `3` low · `4` backlog

### Workflow for AI Agents

1. **Check ready work**: `bd ready`
2. **Claim atomically**: `bd update <id> --claim`
3. **Work on it**: implement, test, document
4. **Discovered new work?** `bd create … --deps discovered-from:<parent-id>`
5. **Complete**: `bd close <id> --reason "…"`

### Rules

- ✅ bd for ALL task tracking; `--json` for programmatic use
- ✅ link discovered work with `discovered-from`
- ✅ check `git status .beads` before assuming a bead write is captured in git
- ❌ no markdown TODO lists, no parallel tracking systems

## Subagent Delegation

When spawning subagents, keep each assignment narrow and bounded.

- Give each subagent a specific scope and a small set of target files or questions
- Bound discovery output; avoid unbounded `rg --files`, broad `find`, or huge file dumps
- Require every subagent to finish with a concise final report
- For important reviews, have subagents also write a short report artifact in
  `/tmp` or the workspace so findings survive a lost final message

## One Repo, One Notebook

This is the ONE source repository, `tedswinyar/banshee`, and it is public. There is
no second copy of the code anywhere: the two-repo scrubbed-mirror design was retired
on 2026-09-13 (ADR-0011) after a review showed the private material was four things
and none of them code. Those four live in **`private/`** — a separate git repository
nested in this checkout, gitignored here, pushed to a private notebook repository:

| In `private/` | |
|---|---|
| `HANDOFF.md` | the session-resume snapshot |
| `beads/issues.jsonl` (+ `interactions.jsonl`) | **the tracker's only durable copy** — `.beads/` here is gitignored and holds only the local Dolt DB |
| `build-server.local`, `build-server.local.md` | the build host's name and details; `scripts/build-server/build-host.sh` sources the first when present |
| `residue-denylist.txt` | terms that must never appear publicly; the gate reads it when present, and it stays out of the tree because the list is itself the leak |
| `docs/research/`, the parity/positioning/launch/lessons-01 docs, `TEMPLATE.md` + stamp, `evidence/` | planning inputs, machine-specific records, template lineage, screenshots |

`private/README.md` has the full table and the restore recipe. Cross-references from
the public tree name these by description ("the maintainer's notebook"), never by a
path that a public clone cannot follow.

**Three rules that replace the mirror:**

1. **A gitignore is a promise; the residue gate is the mechanism.**
   `scripts/lib/residue-gate.py` runs in the scripts suite on every verify over every
   tracked file: a tracked `private/…` or notebook path, a path into a real user's
   home directory, an RFC 1918 address, a non-noreply e-mail, and anything in the
   private denylist all fail the gate.
   `scripts/tests/test-residue-gate.sh` drives it on fixtures AND the real tree. Bead
   IDs are allowed — the tracker is private, but `banshee-abc` is a harmless token.
2. **Never `git add -f` from `private/`.** The gate refuses it, but the point is the
   habit: if something in the notebook has become public-grade, MOVE it (delete from
   `private/`, add here) in a commit that says so.
3. **The public history starts fresh at the cutover** and is permanent from the flip.
   The pre-cutover history is archived privately and is never republished (it is
   not clean).

If you write anything here that names a machine, an organisation, a person, a home
path or a private note, it belongs in `private/` — put it there, and write the public
sentence around it.

## Landing the Plane (Session Completion)

**When ending a work session**, complete ALL steps. "Landed" means: gates
green, beads current, and the work is wherever this repo's work is supposed
to live — **pushed if a remote exists; committed if there is none.** (A
mandate the repo cannot satisfy teaches agents to ignore mandates; check
`git remote -v` before promising a push.)

1. **File issues for remaining work** — anything that needs follow-up gets a bead
2. **Run quality gates** (if code changed) — `./scripts/verify.sh` must pass.
   The pre-push hook runs it anyway, so a red suite blocks step 4.
3. **Update issue status** — close finished work, update in-progress items
4. **COMMIT, and PUSH if a remote exists:**

   Contributors: push your branch to your fork and open an issue; a maintainer
   verifies it through the relay. Maintainer, on the development machine:

   ```bash
   git add -A && git commit -m "…"
   git pull --rebase origin main   # origin is FETCH-ONLY on the development machine
   git push mbp main               # the ONLY push path: relay → GitHub `staging` → CI on the
                                   # build server → `main` fast-forwards on green (docs/build-server.md)
   gh run list -R tedswinyar/banshee --branch staging --limit 1   # watch it promote

   # The notebook — the tracker's ONLY durable copy lives there, and .beads/ is gitignored:
   bd export --include-memories -o private/beads/issues.jsonl
   /bin/cp -f .beads/interactions.jsonl private/beads/interactions.jsonl
   git -C private add -A && git -C private commit -qm "notebook: $(date -u +%Y-%m-%d)" && git -C private push
   ```

   `git push` / `git push origin` FAIL on purpose (origin's push URL is a dead string;
   maintainer decision, 2026-09-13): every commit reaches GitHub through the build
   server or not at all. A bypass that looks like success is the failure this prevents.
   `git status` will say "ahead of origin/main" until CI promotes — that is not an error.
   Both export flags matter: without `--include-memories` every `bd remember` stays in
   the local DB; without `-o` the export goes to stdout and the file stays stale while
   `git -C private status` reads clean.

5. **Update `private/HANDOFF.md`** if the session ends with work in flight — the next
   session (or the handoff skill) resumes from it. It lives in the notebook, not here.
6. **Update whatever tracking the handoff names** whenever you change what is
   committed, pushed, or merged here. Those records point into this repo; nothing
   points back out, so a session that moves the repo without updating them silently
   makes them lie. The notebook's README says what to update.

**No exceptions on the gates:** "the tests pass locally" without a commit is
not landed, and a red suite is never landed.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:6cd5cc61 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.

## Agent Context Profiles

The managed Beads block is task-tracking guidance, not permission to override repository, user, or orchestrator instructions.

- **Conservative (default)**: Use `bd` for task tracking. Do not run git commits, git pushes, or Dolt remote sync unless explicitly asked. At handoff, report changed files, validation, and suggested next commands.
- **Minimal**: Keep tool instruction files as pointers to `bd prime`; use the same conservative git policy unless active instructions say otherwise.
- **Team-maintainer**: Only when the repository explicitly opts in, agents may close beads, run quality gates, commit, and push as part of session close. A current "do not commit" or "do not push" instruction still wins.

## Session Completion

This protocol applies when ending a Beads implementation workflow. It is subordinate to explicit user, repository, and orchestrator instructions.

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Handle git/sync by active profile**:
   ```bash
   # Conservative/minimal/default: report status and proposed commands; wait for approval.
   git status

   # Team-maintainer opt-in only, unless current instructions forbid it:
   git pull --rebase
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->

<!-- BEGIN BEADS CODEX SETUP: generated by bd setup codex -->
## Beads Issue Tracker

Use Beads (`bd`) for durable task tracking in repositories that include it. Use the `beads` skill at `.agents/skills/beads/SKILL.md` (project install) or `~/.agents/skills/beads/SKILL.md` (global install) for Beads workflow guidance, then use the `bd` CLI for issue operations.

### Quick Reference

```bash
bd ready                # Find available work
bd show <id>            # View issue details
bd update <id> --claim  # Claim work
bd close <id>           # Complete work
bd prime                # Refresh Beads context
```

### Rules

- Use `bd` for all task tracking; do not create markdown TODO lists.
- Run `bd prime` when Beads context is missing or stale. Codex 0.129.0+ can load Beads context automatically through native hooks; use `/hooks` to inspect or toggle them.
- Keep persistent project memory in Beads via `bd remember`; do not create ad hoc memory files.

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.
<!-- END BEADS CODEX SETUP -->
