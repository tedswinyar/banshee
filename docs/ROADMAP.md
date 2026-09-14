# Roadmap

The durable, directional view. GitHub issues track the work; this file tracks the
*shape* — where the project is going and what is explicitly out of scope. Update
it when direction changes, not when tasks close.

Where things stand: 0.1.4 is the last built version, and `main` carries everything
listed under `[Unreleased]` in `CHANGELOG.md` — alert episodes, the thermal and
memory dimensions, the who-line, the headroom read and deltas. The next release is
**1.0.0**: the first tagged build cut by the release pipeline rather than by hand.

## Now

- **The 1.0 release.** A rehearsal release end to end on the build server —
  notarize, tag, publish the signed appcast, install from the DMG, then bump and
  watch Sparkle offer the update — before the repository is made public.
- **The agent hook and statusline.** A `banshee hook <event>` for coding agents
  and a statusline glyph, both built on the headroom read: `ask`, never `deny`.
  Not built yet; the read it depends on is.
- **Notifications that respect the human.** Interruption levels, quiet hours, a
  mute window for the Slack sink, and a generic webhook or command sink.
- **MCP polish.** Tool descriptions that say what an agent gets wrong, and a
  top-processes tool so an agent can name a culprit without reading a whole census.

## Next

- **The limits dimension.** Per-process file-descriptor exhaustion (the macOS soft
  cap of 256) and system-wide open-file-table saturation — the failure that looks
  like "the app is broken" and is actually the machine.
- **Census classes.** An idle-spin class (an agent CLI burning CPU with no terminal),
  broken-pipe proof for orphaned stdio helpers, listening ports attributed to their
  stale owners, and a tri-state reap verdict (reap, ask, spare).
- **Per-process swap and compressed accounting.** Resident size is the wrong lens
  for a machine already deep in swap; the swapped-out pages are exactly what it does
  not count.
- **Confidence on projections.** "Full in 1.5 days" reversed within the hour once;
  a time-to-exhaustion line needs a gate on how steady the slope has been.

## Later / someday

- A Homebrew cask with a `binary` stanza for the CLI and MCP server, once a public
  release exists to point it at.
- An Intel or universal build. The release build is arm64-only today.
- A second named projection of the model, if a client ever genuinely needs a
  different reduction — ADR-0005's escape hatch, computed in core and served
  alongside the first.

## Explicitly not doing

Record rejected directions WITH the reason — a future maintainer will re-derive
the idea and needs to know why it lost last time.

- **Disk hotspots — "what is taking the space".** That is a dedicated disk tool's question, and
  Banshee reads through to it rather than walking the filesystem
  ([ADR-0003](adr/0003-sentinel-not-explorer.md)). A sentinel that also explores
  is two apps sharing a menu bar icon.
- **Banding on swap PERCENT.** The denominator moves — macOS grows swap files on
  demand — so the percentage can fall while the machine worsens
  ([ADR-0007](adr/0007-syscalls-not-subprocesses.md)). Absolute swap bytes, the
  swapin/swapout rate, and free memory are the honest signals.
- **A severity computed in any client.** Four surfaces, one reduction
  ([ADR-0005](adr/0005-pressure-model-in-core.md)). The glyph travels over the
  wire precisely so a menu bar showing 😴 cannot coexist with a CLI reporting a
  thrashing machine.
- **Data write routes.** The writer is a timer. A create/update/delete surface for
  readings would exist only to let a client fake one — the one thing a monitor must
  not be able to do. (The reap and backup POST routes are actions, not reading writes.)
- **Coverage percentage as a test target.** The bar is structural and
  mutation-proven instead (see `CONTRIBUTING.md`).
- **A 0–100 health score, memory "purge" tools, an auto-kill mode, an LLM in the
  verdict loop, multi-metric menu bars, network bandwidth, the Mac App Store.**
  Each is argued in the README's "Explicitly not doing" list.
