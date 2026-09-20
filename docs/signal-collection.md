# Signal collection — the faithful port of `perf-scan`

Banshee's samplers reimplement `perf-scan` (386 lines of bash). This document is the spec they implement
against: every signal, the exact command that produced it, the thresholds, and
the parse traps. **It exists so nobody has to re-derive any of this from the
shell script, and so the port can be checked rather than trusted.**

Parity with `perf-scan` on the same machine minute is the acceptance bar before
the script retires to a shim. **That bar was met 2026-09-01** — the parity
reconciliation was done divergence by divergence on that date — and the script is
now a shim (`scripts/perf-scan-shim.sh`, installed by `make daemon-install`).

> **Scope.** Disk *hotspots* are deliberately absent — see
> [ADR-0003](adr/0003-sentinel-not-explorer.md). Banshee reads `statfs` per volume
> and nothing else about the filesystem. Per-path usage is a disk-usage tool's territory.

## Two tiers, and why

`perf-scan` runs `lsof -a -p <pid> -d cwd` once per agent PID and a full
`ps -Ao rss,args` scan per configured app group. That is fine for a command you
run when you're worried. It is a battery and I/O bug every 15 seconds.

| Tier | Cadence | Cost | Contents |
|---|---|---|---|
| **Cheap** | 15s | sub-millisecond syscalls | load, swap, memory pressure, `vm_stat`, boot time, `statfs`, process count |
| **Census** | 5 min | one `ps` walk + `tmux` + selective `lsof` | agent sessions, orphans, app groups, monitoring agents |

`lsof` runs **only** for sessions already flagged stale by the cheap `etime`
test — never for every PID.

## Cheap tier

| Signal | Source | Notes |
|---|---|---|
| Core count | `sysctl -n hw.ncpu` | read once, cache |
| RAM bytes | `sysctl -n hw.memsize` | read once, cache |
| Load 1m / 5m | `sysctl -n vm.loadavg` → fields 2 and 3 | |
| Process count | `ps -Ax \| wc -l` | **`perf-scan` is off by one here** — it counts the header line. Do not reproduce the bug; do note the discrepancy when comparing outputs for parity. |
| Boot time | `sysctl -n kern.boottime` | see trap below |
| Swap total / used MB | `sysctl -n vm.swapusage` | `total = N.NNM`, `used = N.NNM` |
| Available RAM | `host_statistics64`: free + speculative + purgeable + external pages | NOT `vm_stat`'s free list, which macOS keeps deliberately tiny (100–500 MB on any healthy machine). Banding on `pages_free` alone was the memory false-positive bug: red at 84 MB "free" while `kern.memorystatus_vm_pressure_level` said NORMAL. **This is NOT `memory_pressure`'s "free percentage"** — that figure is `kern.memorystatus_level`, the kernel's own reclaimability estimate, and it is much larger (measured 40% vs our 11.5% at the same instant, 2026-09-01 parity run). An earlier revision of this row claimed they track each other; they are different quantities, both honest. |
| Kernel memory pressure level | `sysctl -n kern.memorystatus_vm_pressure_level` | 1 normal, 2 warn, 4 critical. The kernel's OWN verdict, banded as its own dimension since schema v8: the 2026-09-01 parity run measured WARN while available RAM was comfortably green (swap carried the pressure), so it is a signal in its own right, not a restatement of availability. Stored per sample; 0 marks a pre-v8 row ("not recorded" — skipped by the model, never read as normal). |
| Kernel reclaimable % | `sysctl -n kern.memorystatus_level` | The kernel's OWN reclaimability **percentage** — a DIFFERENT quantity from `memorystatus_vm_pressure_level` above (a percent, not a 1/2/4 level) and from availability (measured 40% here vs our 11.5% at the same instant, 2026-09-01 parity). The `KernelFree` **advisory** dimension since schema v13: shown, banded to colour the row, never a bell. Stored per sample; NULL marks a pre-v13 row. |
| Jetsam kills | `sysctl -n kern.memorystatus.kill_on_sustained_pressure_count` | Lifetime counter of processes the kernel killed via the **sustained-pressure** path only (`kern.memorystatus.kill_on_sustained_pressure_count`) — cheap integer read, 0 on a healthy machine. It does NOT count idle-exit kills (`rf:high` or `rf:low`) or per-process-limit kills, which is how the 2026-09-15 crisis killed ~170 processes while this counter stayed 0 — so a zero here means "no *sustained-pressure* kill", not "nothing was killed" (banshee-714). The `Jetsam` dimension since schema v13; a positive DELTA over the window is a confirmed kill, **not** advisory, and rings the top bell AT ONCE (no sustain grace — the kernel already destroyed a process). Delta invalidated across a `boot_time_secs` change (a reboot resets the counter). Stored per sample; NULL marks a pre-v13 row. |
| Kernel thermal pressure level | `notify_get_state("com.apple.system.thermalpressurelevel")` | 0 nominal, 1 moderate, 2 heavy, 3 trapping, 4 sleeping (`OSThermalNotification.h`, the macOS arm of the enum; Foundation's `ProcessInfo.thermalState` collapses it to nominal/fair/serious/critical). The kernel's OWN verdict about heat, banded as the eleventh dimension since schema v11: moderate is yellow, heavy and above red. A throttled machine runs slower for the same load figure, so a load red without this beside it is a story with a missing chapter. Stored per sample; NULL marks a pre-v11 row ("not recorded" — skipped by the model, never read as nominal, because 0 IS nominal). |
| CPU speed limit | `IOPMCopyCPUPowerStatus` → `CPU_Speed_Limit` (percent) | **Absent on Apple silicon**: the call returns `kIOReturnNotFound` and `pmset -g therm` says no CPU power status is recorded (probed 2026-09-08). Only an Intel PMU publishes it. Stored nullable, never banded — when present it is the direct measurement of throttling and rides beside the level for the record. |
| CPU tick counters (total / idle) | `host_processor_info(PROCESSOR_CPU_LOAD_INFO)`, summed across cores | Cumulative ticks: `total` = user+system+idle+nice, `idle` the idle state alone (schema v12). Lifetime counters — **differentiate them** to get MEASURED utilization, see below. Stored nullable; NULL marks a pre-v12 row ("not recorded"). |
| Swapins / swapouts / compressor pages | one `vm_stat` pass | lifetime counters — **differentiate them**, see below. `pages_compressor × page_size` is compressed-bytes occupancy: the `Compressor` **advisory** dimension since schema v13, and the signal that now BACKS the Shrieking reboot finding — before v13 that finding asserted "only a reboot resets a saturated compressor pool" while Banshee measured nothing about the compressor. No new sysctl: the raw signal was already collected here, simply unused by the model. |
| Page size | `vm_stat` → `page size of N` | default 16384 |
| Free bytes per volume | `statfs` | not `df`; see trap below |

### Derived

- **Load per core** = `load1 ÷ ncpu`. **NOT the CPU band** — it is the labeled
  secondary, the run-queue depth. perf-scan banded on it and read
  3.26× per core over cores that were half idle, because XNU's load average counts
  every running AND runnable thread — a churning security agent's blocked threads
  among them — so a deep run queue is not proof the cores are busy. When load per
  core is deep (`> cpu_contention_demand_per_core`, default 3) but measured
  utilization is below yellow, that is **scheduling contention**, not saturation, and
  the copy says exactly that. It is contention at the scheduler's run queue, never
  "I/O wait": XNU's load average does NOT count uninterruptible/I-O-wait threads the
  way Linux does, so that word would be a lie on this platform.
- **CPU utilization** (the band) = `Δ(cpu_ticks_total − cpu_ticks_idle) ÷
  Δcpu_ticks_total` between consecutive samples — the fraction of CPU time actually
  spent doing work, a rate signal (see below). Yellow `≥ 0.85`, red `≥ 0.95`. This is
  the headline CPU number; load per core rides beside it as context.
- **Swap %** = `used ÷ total`. Yellow `≥ 60`, red `≥ 90`. Since banshee-cwl the
  swap detail also reports pool fill as "pool N% full", and the swap FINDING
  projects exhaustion of the allocated pool at the current climb rate — a
  ceiling distinct from the volume's own "full in X". Swapfiles live on the
  data volume, so when the pool rivals free space the DISK finding says "N of
  the volume is swap": the memory→swap→disk→jetsam spiral, named once.
- **Compressed GB** = `compressor_pages × page_size ÷ 1073741824`.
- **Uptime days** = `(now − boottime) ÷ 86400`. Material only alongside high swap:
  long uptime plus pinned swap means fragmented, unreclaimable memory, and
  **only a reboot truly resets the compressor pool.**

### The rate signals — Banshee's whole reason to exist

`perf-scan` can only read *lifetime* `vm_stat` counters, because one sample is
all it has. Banshee differentiates them:

- **swapins/sec, swapouts/sec** from consecutive samples. This is the honest
  thrash signal; the swap *gauge* can look calm while the machine is churning.
  **The thrash dimension bands on the PEAK of a 300s window, not on the newest
  interval** (`thrash_peak_window_secs`). Thrash is a spike train
  rather than a level — one measured afternoon read 3, 24, 257, 1641, 1706, 2329,
  4145 and 6394 ops/sec, swinging ~70× within minutes — so the newest interval
  answers "is it churning this second?" when the question is "has this machine been
  janky?". Banding on the instant made the verdict report GREEN at 24 ops/sec six
  minutes after an alert fired RED at 1706. The statistic is a **trailing median
  filter over 3 intervals, then a max over the time window**. The median is what
  stops one 15-second burst pinning the band — the 6394 reading above was the kernel
  tearing down a 6.5 GB browser that had just been quit, i.e. churn caused BY the
  remediation — and taking the max only afterwards is what makes the figure DECAY
  monotonically as a storm ages out. An upper percentile was tried first and rejected
  in the field: its rank depends on how many points are in the window, so points
  merely leaving moved it to a higher order statistic and the live figure climbed
  118 → 523 ops/sec with no new storming at all. The window is measured in TIME, so a
  hole longer than the window pushes a pre-sleep storm out of scope.
- **CPU utilization** from consecutive samples' tick counters (above). A rate,
  because a single sample's cumulative ticks say nothing about the current instant;
  a pair spanning a reboot or a pre-v12 gap yields no point rather than a wild one.
- **load slope** over a 5-minute window.
- **free-bytes slope per volume** → projected time-to-full.
- **d(cpu_seconds)/dt per monitoring-agent group** — the real answer to "are the
  managed agents getting more expensive?", which a single cumulative reading
  cannot give.

Counters are monotonic **until reboot**. Every rate computation must detect a
boot-time change and discard the delta rather than emitting a huge negative
rate. Same for a counter that wraps.

## Census tier

**Implemented with TWO `ps` calls, joined on pid**, not one:

```
ps -Ao pid=,ppid=,rss=,etime=,time=,tty=,comm=    # identity + costs
ps -Ao pid=,args=                                  # full command line
```

Both `comm` and `args` can contain spaces (`/Applications/Google Chrome.app/…/
Google Chrome Helper`), and two variable-width space-containing fields on one line
cannot be parsed unambiguously — so they cannot share an invocation. `comm` is what
`pgrep -x` matches and what the kernel knows; `args`'s argv[0] is whatever the
process chose to write there. Two spawns per census against `perf-scan`'s ~8.

### Agent CLI sessions

Program identity is `comm`'s basename, matched EXACTLY (`pgrep -x` semantics).

**Interactive vs IDE-spawned is the load-bearing split.** `tty == "??"` or empty
means no controlling terminal, which means an editor or app spawned it (an IDE's
bundled agent CLI, say), not a session someone is sitting in. Those roll up into a single
helper count and MB rather than padding the session list. Without this filter the
list is unusable — one measurement found dozens of subprocesses from a single
agent CLI.

Staleness: `etime` containing a dash means the process is older than 24h, and the
day count is the part before it. **This is the cheapest possible multi-day test.**
Flag at `≥ TMUX_STALE_DAYS` (default 2).

`cwd` via `lsof -a -p <pid> -d cwd -Fn` — **flagged sessions only**.

### Orphaned helper processes

Processes matching `mcp|language.server|tree.sitter` (case-insensitive) with
`ppid == 1`. Their parent session died and left them running.

Measured 2026-08-04: **498 MCP processes / 1.9 GB total, ~45 orphaned.** Every
agent session spawns roughly a dozen.

### tmux sessions

`tmux list-panes -a -F "#{session_name}|#{pane_pid}|#{pane_current_command}|#{session_activity}"`.
Idle days = `(now − session_activity) ÷ 86400`. Guard the whole section on a tmux
server actually running.

### App process groups

Per configured group name: `ps -Ao rss,args`, fixed-string match against the
**full argv**, sum RSS and count. Over-matches by design — any process whose
command line mentions "Docker" counts. Multi-process apps hide their real
footprint, and quit-and-relaunch reclaims swapped pages.

**perf-scan's numbers here are +1 per group**: its pipeline's own
`awk -v n="$app"` carries the group name in argv, survives `grep -v grep`,
and gets counted — so every group gains a phantom ~1 MB process and an
empty group reports 1. Found on the parity run (every group differed by
exactly one); do not "fix" Banshee to match it.

### Monitoring / security agents

Per configured name, from `ps -Ao pid,time,etime,rss,comm`. **Banshee matches
the name against the full command line (argv), where perf-scan grepped `comm`
only** — a deliberate deviation, uniform with the app-group match: on the
parity run the argv match caught a helper process that comm-matching misses,
at the cost of one cosmetic over-match contributing ~0 CPU. The per-group
percentages matched perf-scan to the decimal either way.

```
%core   = Σ cumulative_cpu_seconds × 100 ÷ max(etime_seconds)
cpu_hrs = Σ cumulative_cpu_seconds ÷ 3600
```

**Use cumulative CPU time, never `%CPU`.** `%CPU` is instantaneous and lies about
daemons. The measurement from the reference overload (2026-08-04) is the whole
point: the managed agents on that machine sat at the top of `top` at roughly half of
one core combined — **a few percent of total CPU** — and they were *not* the cause;
a pile of multi-day agent sessions was.

(The agents are named only in the machine-local overlay, never here — see
Configuration below.)

These agents are **event-driven**: they bill per process spawn and per file open.
Cutting process churn lowers their CPU as a side effect. Such agents are usually
not the user's to remove, so that is the only lever available — which is worth
saying in the UI rather than implying the user should uninstall them.

**The two fields have DIFFERENT formats.** `etime` is `[[DD-]HH:MM:]SS` and uses a
dash past 24h. `time` (cumulative CPU) is `MMM:SS.ss` and **never rolls minutes
into hours** — measured `315:45.00` on a process whose `etime` was `21:39:04`.
Reading a 2-part `time` as `HH:MM` turns 18,945 seconds into 1,136,700.

**Two ways this metric misleads, both measured 2026-08-31:**

- **Never sum percentages across agents.** Each group divides by the longest-lived
  process *in that group*, so the denominators differ. A real census summed to
  **59.9%** of one core against an honest deduplicated **49.2%**. Overlapping
  patterns (one pid matching two configured names) double-count on top of that.
  Banshee therefore also reports a `monitorTotal` computed once per process
  against one global denominator — that is the number to show a user.
- **The ratio is noisy for short-lived processes.** A helper restarted 109
  seconds earlier reported **14.6%** off 16 seconds of CPU. The lifetime
  denominator is exposed as `longestLifeSecs` so the pressure model can refuse to
  band on a figure derived from a process that has barely run.

## Configuration

`perf-scan` keeps five arrays plus a local override sourced as bash:

| Name | Purpose | Override semantics |
|---|---|---|
| `APP_GROUPS` | app process-group names | **append** |
| `MONITOR_AGENTS` | security/DLP agent names | **replace** |
| `AGENT_CLIS` | agent CLI binaries (`pgrep -x`) | replace |
| `BUSY_COMMANDS` | glob patterns that mean "mid-build, do not kill" | **append** |
| `ORPHAN_PATTERN` | regex for helper processes | replace |

**Site-specific names must not enter this repo, and that includes prose.**
The machine-local overlay holds that machine's managed-agent process names, app
groups and build-tool busy patterns; none of them belong in a repository. Banshee
loads the overlay from **outside the repository**, which
is a stronger guarantee than a `.gitignore` entry someone has to remember to add.
Two locations are searched, in order:

1. `census.local.toml` in the Banshee directory under the user's
   `Library/Application Support` — the platform convention, and where
   `dirs::config_dir()` points on macOS.
2. `.config/banshee/census.local.toml` in the home directory — the `perf-scan`
   habit, kept as a fallback so a user with a year-old list does not have to
   discover a new directory.

**`dirs::config_dir()` is NOT the XDG `.config` directory on macOS.** An earlier
version of this code searched only location 1 while its doc comment claimed the
XDG path; a real overlay sat unread through an end-to-end run while the log reported `present=false`.

TOML has no `+=`, so each list is spelled twice: `x` replaces, `x_add` appends.
Unknown keys are a loud error — somebody who writes `monitor_agent` (singular) and
sees no complaint would believe their agents were configured, and the symptom is an
EMPTY monitoring section, which reads as good news.

## Parse traps — every one of these cost something

- **`kern.boottime` needs a comma-anchored match.** It prints
  `{ sec = 1784855819, usec = 715641 } ...`; a greedy `.*` captures `usec`
  instead of `sec`.
- **`etime` day counts need explicit base-10.** In shell, `08` is an invalid
  octal literal — hence `10#${et%%-*}`. In Rust, parse with an explicit radix and
  reject rather than silently accepting; the point is that leading zeros are real.
- **tmux capitalizes framework interpreters.** `pane_current_command` reports
  `Python`, not `python3`, so lowercase before matching the busy-command list.
- **`df /` is the wrong volume on APFS.** `/` is the sealed system snapshot. On
  2026-08-20 scanning `/` reported "140Mi avail" while the data volume actually
  had 21Gi. Read the data volume. (APFS purgeable space can also move the number
  on its own, so a single reading is not a promise.)
- **`%CPU` vs cumulative time** — stated above; it is the single most important
  measurement decision in the app.
- **A recycled PID must never be killed.** Any PID list used for an action is
  recomputed at execution time, never carried over from a report.

## Verdict and findings

`perf-scan` emits a finding only from: load > 3×, swap ≥ 90%, uptime > 7d, stale
agent sessions, and orphaned helpers — notably *not* from the app-group or
monitoring-agent sections, which are informational. Banshee's pressure model
([ADR-0005](adr/0005-pressure-model-in-core.md)) generalizes this, but the
recommendation ordering is inherited verbatim because it is ranked by measured
impact:

> reap stale sessions → reap orphans → quit/relaunch the biggest app groups →
> reboot if swap stays pinned

### The who line

Every finding — yellow as well as red — names the top consumers behind it, from
the newest census and nothing else: no new spawn, no new sysctl
(`pressure/who.rs`). Naming the culprit is the action; "35.6 GB of swap in use"
is a symptom and "who: Chrome ×115 at 7.7 GB, claude ×8 at 2.1 GB" is a to-do
list. The rules, per dimension:

| Dimension | Ranked by | Candidates |
|---|---|---|
| memory, swap, kernel pressure, thrash | resident size (RSS) | app groups, agent sessions grouped by program, and the three rollups: `IDE agent helpers`, `orphaned helpers`, `managed agents` |
| cpu, thermal | the RECENT-RATE lens: CPU burned since the previous census ÷ the interval (`census.cpu_consumers`), falling back to the cumulative-CPU rule when there is no predecessor | ANY process the census saw, grouped by app-group name or program basename — not just agent sessions. The fallback names agent sessions by program plus the managed agents as ONE deduplicated entry (`monitor_total`), never the sum of per-group percentages, whose denominators differ |
| agents (stale sessions) | RSS that reaping frees | stale sessions only, by program |
| orphans | the census's own per-program breakdown (count first) | `orphans_by_program` |
| corporate | each group's own rate, RANKED not summed | `monitor_agents` |
| disk, uptime | — | nobody: what is taking the space is a disk tool's question (ADR-0003) |

**Why recent-rate for CPU (`banshee-aen`).** The cumulative-CPU rule named only
agent sessions and the managed-agent rollup, so a machine at 800% busy could show
a CPU finding that accounted for one core and never named Word, a compiler, or
`WindowServer` — the desktop apps and build tools the census sizes but does not
otherwise time. The census now differences each process's cumulative `time=`
between its two most recent cycles and stores the ranked result
(`census::recent_cpu_consumers` → the `census_cpu_consumers` table, schema v14).
This answers "what is hot NOW": a process that burned a core for the last five
minutes outranks one with a large lifetime total that has since gone idle. The
list is empty on the first census after a start or a wake (no predecessor to
difference), and the who-line falls back to the cumulative rule then so it names
someone rather than nobody.

Two guards carry over from the census for the CUMULATIVE fallback: a group
younger than `corporate_min_life_secs` is never named for CPU (a 60-second-old
process at 90% is a startup burst, not a rate), and a group that costs nothing
(zero RSS, zero CPU) is not a consumer. RSS is the honest measure of what a quit-and-relaunch
reclaims and the wrong lens for a machine already deep in swap — the swapped-out
pages are exactly what it does not count; per-process swap/compressed accounting
is the follow-up. The line is composed once in core and rendered verbatim by the
popover, `banshee status`, the MCP `pressure` tool and the alert text
(`… Red for 4m; who: …`), and the episode keeps the who-line at its peak so
`banshee alerts` can still say who it was after the census has been swept.

## Actions and their safety rails

Ported with tests. These rails are the most valuable thing in
`perf-scan` and the easiest to lose in a rewrite.

**`reap_orphans`** — recompute the PID list at execution time (the script's own
comment: the report above may have taken long enough for the set to shift, *and
killing a recycled PID is unacceptable*). `SIGTERM` all → wait 8s → `kill -0`
survivorship check → `SIGKILL` survivors.

**`reap_stale_sessions`** — for each stale tmux session, in order:
1. skip if `session_activity` is unset or 0;
2. skip unless idle days ≥ threshold;
3. **skip if the pane command matches `BUSY_COMMANDS`** (lowercased first) —
   never kill a session mid-build or mid-test;
4. **skip if cwd is a git repo with a dirty worktree** — *"an unstaged fix is not
   recoverable from tmux scrollback"*;
5. `tmux kill-session`.

Killing sessions orphans their MCP servers, so a session reap should suggest an
orphan reap immediately after.

## Measured cost of the cheap tier (2026-08-31)

From `cargo run --release --example sample_throughput` on the 8-core / 24 GB
machine. These are the numbers that settled the storage design.

| Measurement | Value |
|---|---|
| Insert one sample + volume rows | ~170 µs |
| Duty cycle against a 15s tick | **~0.001 %** |
| Read last 1h (240 samples) | ~1.4 ms |
| Read last 24h (5,760 samples) | ~20 ms |
| Retention sweep, steady state (one bucket) | ~1.2 ms |
| Retention sweep, worst case (a full day aging out) | ~110 ms |
| Raw tier on disk | ~418 bytes/sample → **~2.4 MB/day** |
| Rollup tier on disk | ~200 KB/day → **~6.0 MB for 30 days** |
| **Total steady-state footprint** | **~8.4 MB** |

**Consequence:** the single `Mutex<Connection>` is sufficient. Its own doc comment
warns it "does NOT scale to concurrent clients or heavy/slow queries"; at a
0.001 % duty cycle neither condition is close, so the `r2d2` pool that
ADR-0006 left open as a possibility is **not needed** and was not adopted.

Zero subprocesses per cycle, per [ADR-0007](adr/0007-syscalls-not-subprocesses.md).

### A live reading taken during this work

Three consecutive samples, 15s apart, while a Rust release build was running:

```
sampled_at                    load1m   swap_used   free_RAM   swapins   swapouts
2026-08-31T16:26:06.593265Z    40.56     9.54 GB     273 MB   1391786    2083095
2026-08-31T16:26:22.285308Z    63.85     9.80 GB     394 MB   1394609    2103895
2026-08-31T16:26:37.300634Z   163.22    10.66 GB     514 MB   1439572    2198643
```

Load per core 5.1 → 8.0 → **20.4** — a deep run queue, and here a genuinely busy
one (a `cargo build --release` was pegging the cores, so measured utilization would
have confirmed saturation rather than reading contention). This capture predates the
v12 tick counters, so it shows load per core alone; swapins ran at **~3,000/sec** and
swapouts at **~6,300/sec** across the last interval. Much of that load was this
project's own build, which is the honest caveat — but the swap growth (7.08 GB earlier the same day → 10.66 GB) is real,
and it is exactly the accumulation pattern the app exists to catch. Note also
that swap *total* grew during the session, which is the moving-denominator
problem ADR-0007's addendum describes.

## Reference

- `perf-scan` — **now a trampoline to the installed shim**
  (`scripts/perf-scan-shim.sh`, installed beside the daemon binary). The original
  386-line survey it replaced is in the tool's original git history; this document
  remains the spec it was ported against, and the parity reconciliation done
  divergence by divergence on 2026-09-01 is the proof.
- the original tool's README — states three invariants as prose; treat them
  as acceptance criteria: cumulative CPU not `%CPU`; `--reap-tmux` refuses to
  destroy work; interactive sessions only. All three verified on the parity run.
