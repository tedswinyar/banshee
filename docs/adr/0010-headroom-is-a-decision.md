# ADR-0010: Headroom is a decision, not a number — and Banshee never blocks by default

- **Status**: accepted
- **Date**: 2026-09-08

## Context

Banshee's first consumers were people: a menu bar glyph, a popover, a CLI. Its
next consumers are coding agents — Claude Code, Codex, Cursor — that decide, many
times an hour, whether to spawn another subagent, start another build, or run a
test suite in parallel. Those decisions are made on a machine the agent shares
with a human, and made blind: the agent sees neither the menu bar nor the load
average, and when it does look, it looks at raw numbers.

Every system-monitor MCP server surveyed for the 1.0 plan returns raw metrics
with threshold flags: `cpu_percent: 87`, `memory_available_gb: 1.2`,
`high: true`. An agent handed those has to decide for itself what they mean for
the thing it is about to do, and it gets that wrong in two repeatable ways:

1. **It treats "no verdict yet" as headroom.** A fresh daemon, or one that just
   woke from sleep, has not gathered enough samples for a trustworthy verdict.
   An agent that reads an empty or `checking` payload as "nothing is wrong" starts
   work at the one moment nobody is watching. This is the same lie ADR-0005's
   `Checking` level exists to prevent on the menu bar, moved to a new surface.
2. **It re-derives parallelism from raw numbers.** "8 cores at 40% → I can run 5
   more" ignores hysteresis, ignores that a throttled CPU understates load
   figures, ignores that 1.2 GB "available" on macOS is fine and 84 MB "free" is
   also fine, and — worst — reaches a different answer from the one the human
   sees in their menu bar. ADR-0005 forbids exactly this: a severity threshold
   is a fact that must exist in one place.

There is a second, subtler failure on the other side. A monitor that can tell an
agent to stop can also be *made* to stop it — by a false red, by a stale
sample, by a machine that woke into a 17-minute gap (`banshee-s6s`). The
hysteresis and gap work bought a verdict a person can trust at a glance; it did
not buy one that should be allowed to veto a human's work silently. Kubernetes
made the same distinction between a node *condition* (a statement) and a
*taint* (an enforcement), and its node conditions —
`{type, status: True|False|Unknown, reason, message, lastTransitionTime}` — are
the most widely understood machine-readable shape for "here is what is true
about this machine, and since when".

## Decision

**We will serve a `headroom` read that is a DECISION, not a number, and that
never blocks by default.**

1. **A decision, not a number.** `GET /headroom` returns
   `{shouldWait, recommendedParallelism, retryAfterSecs, conditions[]}` plus the
   level it was derived from, the core count it used, and one `reason` sentence.
   `shouldWait` is a boolean an agent can act on without interpreting anything;
   `recommendedParallelism` is how many more concurrent CPU-bound workers the
   machine can absorb right now; `retryAfterSecs` is when to ask again (null
   when not waiting); `conditions` are Kubernetes node-condition shaped, one per
   pressure source plus `Ready`, so an agent that wants the *why* has it in a
   shape it already knows.

2. **A pure function of the verdict the model already produced** (ADR-0005). It
   lives in `banshee-core` (`pressure/headroom.rs`) and reads the `Pressure`
   the sampler just published — level, the CPU and memory readings, the thermal
   band, every band — plus the core count from the newest sample. It is computed
   once per evaluation and served on HTTP, the CLI (`banshee headroom`) and MCP
   (`headroom`) as one parity-table row; no client re-derives any of it. No
   schema change: like `Pressure`, headroom is recomputed from history.

3. **`Checking` is `shouldWait: true` with `recommendedParallelism: 0`.** Headroom
   is never invented before data exists. The MCP description says so in words,
   because the payload alone has not stopped an agent from reading an empty
   answer as a green light.

4. **Never block by default — Banshee informs, the agent's human decides.** A
   hook built on this read should return `permissionDecision: ask` when
   `shouldWait` is true, never `deny`; no such hook ships yet. The read itself
   carries no enforcement semantics: `shouldWait` is advice with a reason attached, and the
   MCP description tells the agent that. A monitor that can silently veto work
   will one day veto it wrongly, and the currency it spends is the trust the
   whole product runs on. Someone who wants a hard block can opt into one; it is
   not what the tool does unasked.

5. **The formula, as shipped (defaults decided 2026-09-08, below).**
   With `cores` = the newest sample's `ncpu`:
   - `cpuSlots = floor(cores × (1 − loadPerCore))`, from the CPU reading's
     hysteresis-smoothed value; `cores` if there is no CPU reading.
   - `memSlots = floor((availableBytes − reserve) / memoryPerSlot)` with
     `memoryPerSlot` **2 GiB** (`PressureConfig.headroom_memory_per_slot_bytes`, env
     `BANSHEE_HEADROOM_SLOT_BYTES`) and `reserve` = **25% of total RAM**
     (`headroom_memory_reserve_fraction`) — the share agents may never fill, held
     for the OS and the human. No cap if there is no memory reading. (Shipped
     first as 1 GiB with no reserve; changed the same night on the prior art —
     see "Decided" below.)
   - `slots = min(cpuSlots, memSlots)`, then capped by level: Quiet → `cores`,
     Stirring → `ceil(cores / 2)`, Restless → `ceil(cores / 4)`.
   - Thermal MODERATE (yellow) halves it (rounding up): the fans are already at
     work and the machine is spending capacity on cooling.
   - **Zero, and `shouldWait`, when:** the level is Wailing or Shrieking (a
     sustained red — the machine is in the state the notification levels exist
     for); or ANY memory-source dimension (available memory, swap, kernel
     pressure, thrash), the thermal dimension, or the disk dimension is RED.
     Memory because adding work adds pressure on the thing already failing;
     thermal HEAVY because the kernel is throttling and the load figures
     understate the work, so the arithmetic above cannot be trusted; disk
     because builds write. CPU red needs no rule — at ≥ 3× per core the
     arithmetic already yields zero. Sprawl and managed-agents reds inside their
     grace period do not zero it: they are worklist items, not capacity.
   - Otherwise a **floor of 1**: a machine that is not in trouble can always run
     one more thing, and this is what makes `shouldWait` exactly
     `recommendedParallelism == 0` — one boolean, one integer, never
     contradicting each other.
   - `retryAfterSecs` is a **fixed ladder per level**, null when not waiting:
     Checking 30 s; a red or a resource zero below Wailing 60 s; Wailing 120 s;
     Shrieking 300 s (`PressureConfig.headroom_retry_*_secs`).

6. **Conditions.** `Ready` (status `True` when the verdict is trustworthy, i.e.
   not `checking`) followed by one condition per source in a fixed order —
   `CpuPressure`, `MemoryPressure`, `ThermalPressure`, `DiskPressure`,
   `SprawlPressure`, `ManagedAgentsPressure`. Status is `True` when any
   non-advisory dimension of that source is red, `False` when there is a
   reading and none is red, `Unknown` when the source has no reading yet (no
   census, pre-v11 thermal). `reason` is a CamelCase token naming the worst
   dimension and its band (`SwapRed`, `CpuYellow`, `AllGreen`, `NoReading`);
   `message` is that dimension's `detail` string from the verdict, verbatim;
   `since` is when the worst dimension entered its band (`evaluatedAt −
   heldSecs`, so a gap in observation truncates it honestly), null when Unknown.

## Consequences

**Easier.** An agent gets an answer, not a dataset: one boolean to branch on,
one integer to size a pool, one interval to sleep, and a shape it already knows
for the reasons. Every surface says the same thing, because there is one
function. An agent hook, when built, is a thin emitter over this read. The
popover can say "room for N more" from the same field the CLI prints.

**Harder / given up.** The formula is a wire contract now: retuning a default
changes fixtures in two languages, which is the friction ADR-0005 accepted on
purpose. `recommendedParallelism` describes CPU-bound workers; an agent whose
subagents are mostly waiting on a network can safely run more, and the read does
not know which kind it is talking to. "Never block" means a hook cannot protect a
machine from an agent whose human always clicks through; that is the human's
call, and the ADR says so. `since` is derived from held time, which the gap
logic already truncates at any hole in the series, so it can be later than the
true transition — the same honesty the verdict itself has.

**Decided 2026-09-08, on published prior art (build-tool job counts, kernel and
Kubernetes pressure thresholds, coding-agent concurrency guidance).** (a) The per-worker
memory budget is 2 GiB, not the 1 GiB first shipped — measured Claude Code workers
run 0.4–0.9 GB before they spawn a build that uses every core, and Gentoo's documented
compile budget is 1.5–2 GB — and 25% of total RAM is held back before dividing,
because every scheduler surveyed reserves a share for the rest of the machine (Bazel
a third, GKE/AKS/EKS 18–25% of a laptop-sized node). No core reserve: the consensus
default is one worker per core governed by load average (`make -l`, `ninja -l`), and
Apple protects the interactive user with QoS, not idle cores. (b) The load
arithmetic, the level caps and the floor of one stay: make and ninja keep one job
running the same way, and Dask's staged memory thresholds are the analog of the
caps. (c) "Any red on memory, thermal or disk means wait" stays: the red band is
hysteresis-confirmed (two consecutive samples, ≥ 30 s), which is systemd-oomd's
window, and Apple says thermal SERIOUS means stop or defer work. (d) The retry
ladder stays. One follow-up is filed under the name "headroom: leave slowly": the
kubelet holds a pressure condition for five minutes after it clears; Banshee already
tracks that window as `recovering`, and treating a recovering dimension like a yellow
for the level cap is the analog.

**Escape hatch.** If a consumer needs enforcement rather than advice, that is a
NEW named projection served beside this one (ADR-0005's escape hatch) with its
own explicit opt-in — not a change to what `shouldWait` means, and not a `deny`
default in the hook.
