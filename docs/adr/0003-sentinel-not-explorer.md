# ADR-0003: Banshee is the sentinel, not the explorer

- **Status**: accepted
- **Date**: 2026-08-31

## Context

Two apps by the same author look, from a distance, like they do the same thing.

A dedicated disk-space visualizer is a storage-visualization app: it walks the
filesystem to build a treemap and a hotspot classifier — a registry of known
space sinks, a category enum (regenerable artifact, cache, tool-managed cache,
stale project artifact, review-first, won't-regenerate), source-mtime dormancy
analysis, and hardlink deduplication — show and classify, never delete.

Banshee watches system pressure, and "the disk is filling up" is unarguably a
kind of pressure. So does disk belong to Banshee, to the disk visualizer, or to both?

Answering by *resource type* ("disk is the visualizer's") makes Banshee blind to a real
and recurring failure mode: on 2026-07-16 this machine reached 1.1 GB free, and
nothing warned. Answering by *both* means two hotspot classifiers, which is the
duplication that prompted the question in the first place.

The useful cut is neither. It is the **job to be done**, and the two apps have
genuinely different ones — different loops, different cadences, different shapes
of answer:

|  | Disk visualizer | Banshee |
|---|---|---|
| Question | "Where did my bytes go?" | "Is this machine about to fall over?" |
| Shape | Spatial — treemap, per-path | Temporal — time series, rate of change |
| Cadence | On-demand, expensive full walk | Continuous, cheap sample |
| Targets | Files and directories | Processes, sessions, volumes |
| Verb | Explore | Alarm |

A smoke detector is not a duplicate thermal camera, even though both are about
heat.

## Decision

We will treat Banshee as the **sentinel** and the disk visualizer as the **explorer**, and
enforce the boundary with a rule that is checkable rather than aspirational:

**Banshee never walks the filesystem.** Its disk signals come from `statfs` on
mounted volumes — O(volumes), not O(files). Banshee has no treemap, no per-path
sizes, and no reclaimability category enum, ever.

Concretely:

- Banshee owns free-space **trend**: bytes free per volume, rate of change, and
  projected time-to-full. These are the sentinel's questions and they are
  answerable from a handful of syscalls.
- When the question becomes "what is taking the space," Banshee does not answer
  it. It either deep-links to the disk visualizer, or reads through to its
  `GET /scans/{id}/hotspots` and displays the result **attributed to that tool**,
  degrading gracefully when it is not installed or not running.
- Conversely, the disk visualizer does not sample continuously and does not own a menu bar
  item. Alarming is Banshee's job.
- The dedicated disk-usage tool stays untouched by Banshee's work; that is its
  territory. The `perf-scan` script is Banshee's, and retires to a shim once parity is
  proven.

A copy of this ADR is kept with the disk visualizer, so neither project can
drift across the boundary while believing it is the only one that owns it.

**The explorer is the disk visualizer at `com.tedswinyar.phantom`**, a sibling
app of `com.tedswinyar.banshee`. This ADR names the hand-off by bundle id; it
does not overturn the no-walk rule (banshee-60q). Concretely: the disk finding
ships action `openDiskTool`, and a client that can launch apps (the macOS app)
turns it into a button that opens the visualizer when it is installed, falling
back to the finding's text otherwise — never a dead button. The CLI and MCP
keep the generic action label, which is the same words on every surface
(ADR-0005), because the wire is public and must not carry the sibling's name.
Banshee still walks no filesystem; it points at the tool whose job that is. The
read-through to the visualizer's `/volume` and hotspots routes remains the
additive, degrade-gracefully step described below.

## Consequences

- **The rule is testable, not just stated.** "No filesystem walk" is a property
  a reviewer can check by grepping for directory traversal in `banshee-core`.
  A boundary expressed only as a paragraph of intent decays; this one fails review.
- Banshee's disk sampling is cheap enough to run every 15 seconds without a
  battery or I/O cost, which is exactly what the sentinel role needs and what a
  walk could never be.
- **Cost: Banshee's deep disk answer depends on another app.** If the disk
  visualizer is not running, Banshee can say "disk is filling, 3 days to full"
  but not "because a build cache grew 40 GB." That is an acceptable trade — the
  alarm is the load-bearing part, and the read-through is additive.
- This is the first cross-app call in the constellation. Every prior
  constellation hop was client → its own hub. If cross-app reads spread, the
  loopback key-file auth model needs revisiting (each app generates its own key,
  so Banshee must read the other app's key file, which is a wider grant than
  ADR-0001's trust boundary contemplated). Revisit at the second such link.
- **Escape hatch:** if the split ever produces a genuinely confusing user
  experience — two menu bar items arguing about disk, or a disk visualizer that
  nobody opens because Banshee already said enough — the reversal is to fold Banshee's
  disk dimension out entirely and let the disk visualizer own low-space alerting, *not* to
  let Banshee grow a classifier. The no-walk rule is the part worth keeping.
