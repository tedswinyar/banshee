# ADR-0006: Retention and downsampling are designed in from migration 1

- **Status**: accepted
- **Date**: 2026-08-31

## Context

Most apps store records a person created. Their databases grow at human speed,
and "we'll worry about size later" is a defensible position because later may
never come.

Banshee stores things a *timer* created. At the planned cheap-tier cadence of one
sample every 15 seconds, that is 5,760 samples a day, 2.1 million a year, before
counting the per-dimension rows and the census-tier process inventories hanging
off each one. A monitoring database does not grow at human speed and does not
stop growing when you stop using the app — a forgotten LaunchAgent keeps writing
for months.

There is a specific irony to get ahead of: Banshee exists because unattended
processes silently consumed this machine's resources for days. An unattended
Banshee that silently consumed disk would be the same bug wearing the app's own
uniform, and a disk-usage tool would eventually find it sitting in
`~/Library/Application Support/banshee/`.

Retention is also not a thing that can be bolted on. The rollup table has to
exist before there is data worth rolling up, and adding a downsampling scheme
after a year of raw samples means a migration over millions of rows — exactly the
class of migration that ADR-0001's forward-only, never-rewritten schema policy
makes expensive.

## Decision

We will ship retention in the **first** schema migration, not a later one:

| Tier | Resolution | Kept for |
|---|---|---|
| Raw samples | 15s | 24 hours |
| Rollups | 5 minutes | 30 days |
| Alert events | per event | 1 year |

A vacuum-and-rollup pass runs on the sample loop, not on a separate schedule, so
it cannot be independently disabled or forgotten, and so it runs on exactly the
machines that are producing data.

**Amended 2026-09-08 — rollups are continuous, not deferred.** The pass summarises
every bucket that has CLOSED, including the ones still inside the raw window, and
only DELETES raw samples at the raw retention. The tiers therefore overlap for the
last day rather than abut. The original "roll up when aging out" reading left
`/rollups` with nothing younger than 24 h, which made the app's 5-hour and 24-hour
History windows empty on every machine forever; the History chart is the reason
the rollup tier exists at all. The bucket that holds `now` is still never
summarised early — a partial average recorded as final is the one thing the
old rule got right, and it is kept.

The retention windows are configuration. The existence of retention is not.

`banshee-core` exposes the current database size, and Banshee reports its own
footprint in the app. A monitor that will not tell you what it costs has no
standing to complain about anything else.

## Consequences

- Steady-state size is bounded and predictable rather than a function of how long
  ago the LaunchAgent was installed.
- 24 hours of raw resolution is enough for the questions the app actually asks
  ("has swap climbed in the last 40 minutes?"), and 30 days of 5-minute rollups
  is enough for the slow ones ("are the managed agents getting more expensive
  month over month?").
- **Cost: the pre-rollup window is the resolution ceiling for forensics.** Asking
  "what exactly happened at 03:00 last Tuesday" gets a 5-minute average, not the
  15-second detail. Accepted: the app's job is to prevent that morning, not to
  reconstruct it. Alert events keep their full detail for a year precisely so the
  *interesting* moments survive at full fidelity.
- Writing on a timer while the UI polls argued against the template's single
  `Mutex<Connection>`, whose own comment concedes it "does NOT scale to
  concurrent clients or heavy/slow queries". Measured: an insert takes ~170 µs
  against a 15-second tick, a 0.001 % duty cycle, so the single connection was
  kept and no `r2d2` pool was adopted (`docs/signal-collection.md` records the
  measurement).
- **Escape hatch:** if 30 days proves too short to see the trends that matter,
  add a third tier (hourly, kept a year) as an additive forward migration — the
  rollup machinery will already exist, which is the entire reason for building it
  on day one rather than day 400.
