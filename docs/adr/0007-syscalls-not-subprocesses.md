# ADR-0007: The cheap tier reads syscalls, never subprocesses

- **Status**: accepted
- **Date**: 2026-08-31

## Context

`perf-scan` gathers everything by shelling out: `sysctl -n vm.loadavg`,
`vm_stat`, `memory_pressure`, `df`, `ps`, `tmux`, `lsof`. That is the right
design for a script you run when you are already worried — it is legible,
trivially debuggable, and the cost is paid once.

Banshee runs its cheap tier every 15 seconds. Ported literally, that is five or
six `fork`/`exec` pairs per sample: **roughly 28,800 process spawns a day.**

Which runs directly into `perf-scan`'s own most valuable finding. From
`docs/signal-collection.md`, measured 2026-08-04:

> These agents are **event-driven**: they bill per process spawn and per file
> open. Cutting process churn lowers their CPU as a side effect.

The managed agents on this machine all cost CPU in proportion to how many
processes appear on it. (Their process names
live only in the gitignored machine-local overlay, never in this repo.) A monitor that spawns 28,800
processes a day to watch for resource pressure **would itself be a measurable
source of the pressure it reports**, and would inflate one of its own
dimensions. There is a name for that failure and it is not a subtle one.

A monitoring daemon has an obligation the script it replaces did not: it must be
cheap enough that its own presence does not change the reading.

## Decision

The **cheap tier reads the kernel directly** — no subprocesses:

| Signal | Mechanism |
|---|---|
| load average | `sysctlbyname("vm.loadavg")` → a `loadavg` struct (`ldavg[3]`, `fscale`) |
| core count, RAM, page size | `sysctlbyname` on `hw.ncpu`, `hw.memsize`, `hw.pagesize` |
| boot time | `sysctlbyname("kern.boottime")` → `timeval` |
| swap total/used/avail | `sysctlbyname("vm.swapusage")` → `libc::xsw_usage` |
| page counts, swapins, swapouts, compressor | `host_statistics64(HOST_VM_INFO64)` → `libc::vm_statistics64` |
| per-volume free space | `statfs(2)` |

Notes on the bindings, because they took several attempts:

- `libc::host_statistics64` exists and is **not** deprecated. `libc::mach_host_self`
  **is** deprecated, and this project runs clippy with `-D warnings`, so the host
  port comes from `mach2::mach_init::mach_host_self()` instead. That split — one
  call from `mach2`, the rest from `libc` — is deliberate, not an oversight.
- `libc` does **not** define a `loadavg` struct on macOS. We declare our own
  `#[repr(C)]` equivalent. `vm.loadavg` returns fixed-point values that must be
  divided by `fscale` (2048 on this machine); reading `ldavg` without dividing
  yields load figures in the thousands.
- `HOST_VM_INFO64` is not exported by either crate as a plain constant (`mach2`
  has only the `_COUNT` derivations), so it is declared locally as `4`.

The **census tier keeps its subprocesses** (`ps`, `tmux`, selective `lsof`).
There is no clean syscall equivalent for "what is every process's argv and
controlling terminal", `libproc` is a worse trade than one `ps` call, and at a
5-minute cadence it is ~288 spawns a day — three orders of magnitude below the
cheap tier, and comparable to what a human running `perf-scan` a few times a day
already costs.

## Consequences

- A sample costs microseconds and zero process spawns, so the 15s cadence is
  defensible and the observer effect is negligible.
- **The testing standard's "parsers start from raw bytes" rule does not apply to
  the cheap tier**, because there are no bytes to parse. The test burden moves to
  the *derivation* layer — bands, rates, boot-change detection — driven from
  synthetic struct values. That rule still fully applies to the census tier,
  which is where the real parse traps live, and where fixtures of captured
  `ps`/`tmux` output are mandatory.
- **Cost: `unsafe` in `banshee-core`.** Confined to one module (`sys.rs`) behind a
  safe trait so the rest of the crate — and every test — stays safe and mockable.
  The trait is also what makes the sampler testable without a real machine.
- Parity checking against `perf-scan` gets harder in one specific way: we are no
  longer reading the same *text*, so a divergence could be either a real bug or
  the script's own parsing. When reconciling, compare against the raw
  `sysctl`/`vm_stat` output rather than against the script's derived figures.
- **Escape hatch:** if a future signal has no syscall path and genuinely belongs
  in the cheap tier, move it to the census tier rather than adding a spawn to the
  15s loop. If it belongs in neither, that is evidence it is not a pressure
  signal.

## Addendum: what this exposed about swap percentage

Verified while validating these bindings: `vm.swapusage` reported
`total = 8.59 GB, used = 7.08 GB` (82.4%). On 2026-08-04 the same field read
**35.6 / 36 GB**. macOS grows and shrinks its swap files on demand, so
**swap-percent-used has a moving denominator** — and the failure mode is
perverse: when the system is under enough pressure that macOS *grows* the swap
file, the percentage can **fall** while the situation worsens.

`perf-scan` bands on swap percentage (yellow ≥60, red ≥90) and therefore
inherits this. Banshee keeps the percentage as a familiar display value but must
**not** band on it alone. The `memory` dimension is defined on:

- **absolute swap bytes used** (monotonic in badness, no denominator),
- **swapin/swapout rate** (the honest thrash signal), and
- **free + compressed page counts** (what the pressure actually is).

Recorded here rather than only in `signal-collection.md` because it is a
deliberate, load-bearing **divergence from the port**, and someone comparing
Banshee to `perf-scan` will otherwise read it as a bug.
