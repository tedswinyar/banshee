# Banshee

A proactive system-pressure sentinel for macOS. It watches the machine
continuously and tells you *before* it falls over — a menu bar glyph that encodes
both how bad it is and which pressure source is winning, a window for the detail,
and an MCP server so agents can ask.

<p align="center">
  <img src="docs/images/popover.png" width="346" alt="The menu bar popover: the verdict, the dimensions out of band, and the best action">
</p>

<p align="center">
  <img src="docs/images/window-verdict.png" width="360" alt="The Verdict tab: findings, best action first, each naming who is responsible">
  <img src="docs/images/window-history.png" width="360" alt="The History tab: five-minute rollups of load with the bucket maximum drawn over the average">
</p>
<p align="center">
  <img src="docs/images/window-census.png" width="360" alt="The Census tab: stale agent sessions, tmux, orphaned helpers, the biggest apps and the managed agents">
  <img src="docs/images/window-alerts.png" width="360" alt="The Alerts tab: alert episodes with their peak, who was responsible and how long each ran">
</p>

*Every name and number in these screenshots comes from the shared test fixtures
(`scripts/fixture-server.py` serves them to the real app), not from a real machine.*

Banshee grew out of a one-shot shell survey the author ran whenever the machine
was already slow. That script only ever answered the question *after* it hurt: by
the time anyone looked, the load average was in the dozens, swap was full, and the
cause — days of forgotten agent sessions and hundreds of orphaned helper processes —
had been accumulating silently the whole time. **Every interesting signal here is a
rate, and an on-demand script structurally cannot compute one.** That is the whole
reason this is a daemon.

```
🫧  Checking      not enough data yet — NOT a claim that anything is fine
😴  Quiet         nothing over threshold
🫥  Stirring      one dimension uneasy
😧  Restless      several uneasy, or a red spike
😱  Wailing       a red sustained past its grace period
💀  Shrieking     two dimensions in sustained crisis
```

At Restless and above the glyph gains the dominant source:
🔥 CPU · 🌡️ thermal · 🧠 memory · 💽 disk · 🕸️ agent sprawl · 🏢 managed agents.

`Checking` is a separate state from `Quiet` on purpose. Without it the menu bar
flashes "nothing is wrong" for the first moments after launch, which teaches the
user to distrust the one glyph that has to be trusted.

For the same reason the app never presents an old verdict as a current one. If the
daemon refuses the app's key the menu bar shows 🔒 at once; if the daemon stops
answering, the last glyph stands for a minute (a restart, a laptop waking) and then
gives way to 🫧. The popover keeps the last verdict, labelled as the last one seen.

## Install

Download the DMG from the
[GitHub Releases page](https://github.com/tedswinyar/banshee/releases),
drag Banshee.app to `/Applications`, and launch it. The first launch offers to
install the daemon (a launchd LaunchAgent, from helpers bundled inside the app);
accept, and the menu bar glyph appears once the first samples are in. On that first
launch from `/Applications` the app also adds itself to your Login Items, so the glyph
comes back after a restart (macOS tells you; Settings › "Open at login" or System
Settings › Login Items turns it off). The daemon starts at login regardless — it is a
background service the app only reads from. Updates arrive through the app's own
"Check for Updates…" (Sparkle, signed appcast).

**Requirements.** macOS 14 (Sonoma) or newer on Apple silicon (the release build
is arm64-only; an Intel or universal build is not produced yet). The DMG carries
the app, the daemon, the CLI and the MCP server; no developer tools are needed on
the machine.

To build from source instead, see [Build & run](#build--run) below.

## Ask it

```
$ banshee status
😧🧠  Banshee: Restless, memory

  !  CPU utilization       88% busy; 1.7× per core  (held 0s)
  !! Free memory           89 MB free  (held 0s)
  !! Swap in use           16.6 GB in use  (held 0s)
  !  Disk headroom         41.3 GB free, full in 5.2 hours  (held 0s)

Findings, best action first:
  1. Only 89 MB of RAM free.
     → Quit and relaunch the biggest apps
  2. 41.3 GB free on the data volume.
     → Open a disk-usage tool to see what is using the space
```

Nine reads, each on three surfaces — HTTP, the `banshee` CLI, and an MCP tool:
`status`/`pressure` (the verdict), `headroom` (the verdict reduced to a decision for
agents: wait or go, how many more workers, when to ask again — advice, never a
block), `samples` (the raw 24h series), `rollups` (5-minute buckets, 30 days),
`census` (what is actually running), `alerts`, `deltas` (what changed over a
lookback, or since an alert episode opened), `stats` (what Banshee itself costs),
`health`. The three must return **byte-identical** JSON,
and `tests/e2e/run-e2e.sh` fails the build if they do not. The data endpoints are all reads — the
sampler is the only writer; a few authenticated POST routes run local actions
(session/orphan reap preview + execute, backup), never data writes.

**The severity is decided once, in `banshee-core`, and travels over the wire —
glyph included.** No client re-derives a level or a band, because that is how
three surfaces come to disagree about whether the machine is on fire
([ADR-0005](docs/adr/0005-pressure-model-in-core.md)).

## Architecture

A constellation: a Rust core owns
the domain, an Axum API on **18769** is the only database writer, and the CLI,
MCP server and SwiftUI app all speak HTTP to it. The API runs as a launchd
LaunchAgent so sampling outlives a closed window, a quit app, and a reboot — the
app is a *client* of it, not its supervisor
([ADR-0004](docs/adr/0004-collector-as-launchagent.md)).

```bash
make daemon-install    # install, or UPDATE after any change (idempotent)
make daemon-status     # what is actually running, and what is wrong with it
```

**Rebuilding does not update the running daemon; reinstalling does.** The
LaunchAgent runs an installed copy, never a build artefact
([ADR-0004](docs/adr/0004-collector-as-launchagent.md)). See
[docs/daemon.md](docs/daemon.md).

| Tier | Cadence | Cost | What it reads |
|---|---|---|---|
| Cheap | 15s | syscalls only, ~170 µs | load, memory, swap, thrash rate, free space |
| Census | 5 min | 2 `ps` + 1 `tmux` + selective `lsof` | agent sessions, orphans, app groups, managed agents |

The cheap tier deliberately spawns **no** subprocesses: ported literally,
`perf-scan`'s shell-outs would be ~28,800 process spawns a day, and event-driven
agents do work on every process spawn and file open, so spawn count is a cost
driver. A monitor that
inflated one of its own dimensions would be a bad joke. See
[ADR-0007](docs/adr/0007-syscalls-not-subprocesses.md).

Steady-state footprint: **~8.4 MB** (24h of raw samples + 30 days of rollups).
The daemon reports its own cost on `/stats` and in `banshee stats` — physical
footprint and CPU as a lifetime average over its uptime, not a burst reading. On
the development machine that is about **5 MB and a fifth of a percent of one
core**; a monitor that will not say what it costs has no standing to complain.

## Privacy

**No telemetry, no analytics, no account.** Every process here talks only to the
local daemon over a Unix socket. Two things reach the network, and nothing else:

- **The daemon** never dials out unless you configure a Slack webhook for
  escalation alerts. Off by default.
- **The app** checks this repository's GitHub Releases for updates on Sparkle's
  schedule (signed appcast, EdDSA-verified package). This is **on by default** in a
  release build, because an update check is how a fix reaches you; turn it off with
  `defaults write com.tedswinyar.banshee SUEnableAutomaticChecks -bool NO`, and use
  "Check for Updates…" by hand. The check discloses the app's version to GitHub and
  nothing else.

The full list of hosts contacted, the trust model, and how to report a
vulnerability are in [SECURITY.md](SECURITY.md).

## Scope

Banshee is the **sentinel**, not the **explorer**. Banshee never walks the
filesystem — disk signals come from `statfs` per volume, O(volumes) not
O(files) — and hands "what is taking the space" to a disk-usage tool.
[ADR-0003](docs/adr/0003-sentinel-not-explorer.md) draws that line.

## Explicitly not doing

Each of these was considered and rejected, with the reason, so the idea does not
get re-derived without meeting the argument that beat it last time. The longer
list with rationale is in [docs/ROADMAP.md](docs/ROADMAP.md).

- **A 0–100 health score.** One number hides which dimension is failing and
  invites tuning the number instead of the machine. The verdict is a level plus
  the source that is winning, and the ranked worklist says what to do.
- **Memory "purge", "clean" or "optimise".** macOS reclaims its own caches; a
  tool that forces it trades a real signal for a cosmetic one.
- **An auto-kill daemon mode.** Every reap is preview-then-confirm, by a person,
  against the set they just read. A monitor that kills on its own is the incident.
- **An LLM in the verdict loop.** The severity is a deterministic replay of stored
  history ([ADR-0005](docs/adr/0005-pressure-model-in-core.md)); the MCP server is
  how agents *ask*, never how the verdict is *made*.
- **Widgets, multi-metric menu bars, fan RPM, SMC writes, private IOReport.**
  The glyph is the product; the rest is a different app.
- **Network bandwidth.** There is no honest capacity number on Wi-Fi to band
  against.
- **Disk hotspots — "what is taking the space".** A dedicated disk tool's job;
  Banshee never walks the filesystem ([ADR-0003](docs/adr/0003-sentinel-not-explorer.md)).
- **Mac App Store and `brew services`.** The daemon is a LaunchAgent the app
  installs and the CLI checks; sandboxing forbids the first and the second would
  supervise it a second way ([ADR-0004](docs/adr/0004-collector-as-launchagent.md)).

## Build & run

Building needs Rust, the Swift toolchain and `jq`; `cargo-deny` and `gitleaks`
for the full verify gate (`scripts/doctor.sh` checks).

```bash
./scripts/verify.sh     # THE gate: rust (fmt + test + clippy), swift, e2e, scripts
make build              # debug build, Rust + Swift
make install            # ship daemon AND app to this machine, from one commit
make daemon-install     # the daemon half alone, as a LaunchAgent
./scripts/start.sh      # a throwaway dev-profile API in the foreground (its own socket + port 18779)
./scripts/warm-dev-db.sh          # let it gather enough history to judge
cargo run --release --example sample_throughput   # measure the sample loop
```

Machine-local configuration — including managed-agent process names, which must
never enter this repo — lives in `census.local.toml` under the Banshee
Application Support directory (or under `.config/banshee`). See
[docs/signal-collection.md](docs/signal-collection.md).

## Documentation

**[docs/README.md](docs/README.md)** — complete documentation map. Find the right doc in <30s:

- First time? Start: [docs/getting-started.md](docs/getting-started.md)
- Running it for real? Read: [docs/daemon.md](docs/daemon.md)
- Want to know what it measures, and how? Read: [docs/signal-collection.md](docs/signal-collection.md)
- Changing something? See: [docs/adding-a-field.md](docs/adding-a-field.md), [VERSIONING.md](VERSIONING.md)
- Shipping a release? Read: [docs/build-pipeline.md](docs/build-pipeline.md), [docs/release-checklist.md](docs/release-checklist.md)
- Where it is going, and what it will not do: [docs/ROADMAP.md](docs/ROADMAP.md)
