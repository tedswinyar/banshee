# ADR-0005: The pressure model lives in banshee-core and travels over the wire

- **Status**: accepted
- **Date**: 2026-08-31

## Context

Banshee reduces a pile of raw numbers — load average, swap percentage, process
counts, per-agent CPU seconds, free bytes per volume — into a single answer a
person can read at a glance: a severity level and the source that is driving it.
That reduction is the product. The raw numbers are almost worthless without it;
`top` already shows raw numbers and nobody watches `top` all day.

There are four surfaces that need the answer: the menu bar glyph, the app's
detail window, the CLI, and the MCP server. The tempting shape is for each to
compute what it needs from the samples it fetched — the menu bar only needs a
level, so why ship it anything more?

Because that is how three surfaces come to disagree about whether the machine is
on fire. Two places that independently know a threshold will eventually disagree
about it, and a severity threshold is a fact with worse consequences than most: a
menu bar showing 😴 while `banshee status` says the machine is thrashing does not
just look sloppy, it destroys the reason to trust the glyph at all. The client
renders what core decided; the view never branches on a raw number.

## Decision

We will compute the entire pressure model in `banshee-core` and serve the result
over the API. Specifically, `banshee-core` owns:

- the dimension definitions (`cpu`, `memory`, `agents`, `orphans`, `corporate`,
  `disk`, `uptime`) and their yellow/red bands,
- hysteresis — enter at threshold, leave at threshold − margin, require N
  consecutive samples,
- the severity level (`Quiet` / `Stirring` / `Restless` / `Wailing` / `Shrieking`),
- the dominant-source selection,
- the **glyph** for each level and source, and
- the findings and recommended actions.

No client re-derives any of these. The menu bar renders a string the API gave it.
The CLI prints a level the API decided. The MCP server reports a severity it was
told. A client that wants to show something the model does not expose is a
request to extend the model, not a licence to compute locally.

Thresholds, margins, and grace periods are configuration, not constants — but the
*code that applies them* exists once.

## Consequences

- The four surfaces cannot disagree, because there is nothing for them to
  disagree about. This is the same guarantee ADR-0001 bought for the database,
  applied to the derived state.
- **Glyphs go over the wire.** Slightly surprising — presentation usually belongs
  to the client — but the alternative is a Swift `switch` and a Rust `match` that
  must be kept in step by hand, and the whole point of the glyph is that it means
  the same thing everywhere. Vocabulary and layout still belong to DesignKit; the
  level→glyph *mapping* does not.
- An MCP agent gets the same reduced answer a human sees, so "what does the
  menu bar say right now?" is answerable without an agent reimplementing
  threshold logic and getting it subtly wrong.
- **Cost: the model is a wire contract**, so changing a band or renaming a level
  is a client-visible change and needs the fixtures updated. That friction is
  the feature — a severity scale that drifts silently is worse than one that is
  slightly annoying to change.
- The reduction is where the bugs will live, so it carries the heaviest test
  burden in the project: mutation-proof tests on band edges, hysteresis
  transitions, dominant-source selection, and the level→glyph map, with parser
  tests starting from raw captured `ps` / `vm_stat` / `tmux` bytes rather than
  from values round-tripped through our own encoder.
- A `Checking` state exists alongside `Quiet` and is not the same thing. Without
  it the popover flashes "nothing is wrong" for ~40 ms on every open, which
  teaches the user to distrust the one glyph that must be trusted.
- **Escape hatch:** if a client ever genuinely needs a different reduction (a
  hypothetical always-on ambient display wanting three states, not five), the
  move is a second named projection computed in `banshee-core` and served
  alongside the first — not a local computation in that client.
