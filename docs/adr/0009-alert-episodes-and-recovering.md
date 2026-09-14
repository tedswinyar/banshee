# ADR-0009: Alert episodes and the Recovering state

- **Status**: accepted
- **Date**: 2026-09-07

## Context

Banshee's alerts are one-shot point EVENTS. Each `AlertEvent` is a moment —
`{id, occurredAt, dimension, band, level, message}` — appended to the
`alert_events` table and rate-limited to one per dimension per hour
(`alert_cooldown_secs = 3600`). That model loses the shape of an incident, and
three measured failures on this machine trace to the same root cause:

1. **Intensity is erased (`banshee-nvd`).** On 2026-09-05 thrash alerts landed at
   17:26, 18:39, 20:08, 21:26 — exactly the one-per-hour cap. Whether that hour
   held one storm or forty is unrecoverable from the record. Because the alert
   log is "the long memory" (samples age out at 24h, alerts are kept a year),
   the lost intensity is lost permanently.
2. **The long memory is invisible where operators look (`banshee-qoz`).** The same
   afternoon, a 12-alert history told the real story — swap ratcheting 12.0 →
   13.8 → 13.9 → 14.0 GB over four hours with thrash interleaved — but `status`
   showed only a point-in-time verdict. An assistant read the verdict, drew the
   wrong conclusion, and found the story only after separately deciding to query
   `/alerts`.
3. **There is no recovery signal, and flapping is mishandled.** A red condition
   that clears is silent; you never learn it is over. A value oscillating around
   the red threshold either spams (short cooldown) or, once hysteresis holds the
   band, goes quiet in a way indistinguishable from genuine calm. And two
   dimensions describing ONE underlying event — free memory collapsing WHILE the
   compressor thrashes — alert independently, doubling the noise for a single
   incident.

Prior art solves each piece. Netdata raises and clears alarms on an **asymmetric
`delay: up U down D`** — slow to fire, slow to clear — so a brief spike never
alarms and a brief dip never resolves. Prometheus has **`for`** (a condition must
hold before firing) and **`keep_firing_for`** (an alert stays firing for a grace
period after the condition clears, so a flap is one alert, not many).
Alertmanager has a **`repeat_interval`** (re-notify while still firing) and
**inhibition rules** (a firing alert mutes a related one). None of this needs new
signals — only a richer model of the alert lifecycle over the signals we already
band.

## Decision

**We will model alerts as EPISODES rather than events, with an explicit lifecycle
state machine, and we will derive that lifecycle as a pure function of stored
history — never from in-process state.**

1. **An episode is one continuous incident on one dimension.** Its durable shape
   is `{id, dimension, startedAt, endedAt?, peak{band, level, severity, at},
   censusAtPeak?, suppressed, state}`. It is OPEN while `endedAt == null`. The
   peak carries the worst band/level/severity seen and when, so "4 storms vs 40"
   and "peaked at Shrieking at 20:08" are both answerable from one row.

2. **Lifecycle state machine, per dimension.** `Quiet` → (red sustained ≥
   `episode_up_secs`) → **Firing** → (recovered below red, sustained ≥
   `episode_down_secs`, i.e. `keep_firing_for`) → **Recovering** → **Closed**
   (`endedAt` set, exactly one recovery notice). A re-reddening while `Recovering`
   REOPENS the same episode: a flap is one incident, not two.

3. **Asymmetric delay.** `episode_up_secs` gates raising (it reuses the existing
   sustained-red machinery — `held_secs`, `red_grace_secs` — so the fire is only
   as trustworthy as the observation behind it, and a gap in the series is never
   counted as continuity).
   `episode_down_secs` is `keep_firing_for`: the episode stays Firing/Recovering
   until the dimension has been below red for that long. Both live in
   `PressureConfig` and are read through `from_env`, so the config is exercised
   by the same path production uses.

4. **Exactly one recovery notice**, emitted on Firing → Closed (via Recovering).
   An episode that never crossed `episode_up_secs` — a spike that self-resolved —
   never fired and so has no recovery notice. Silence for a non-event.

5. **A long repeat interval while firing.** While an episode stays Firing it
   re-notifies at `episode_repeat_secs` (default 3600, today's cooldown), NOT once
   per evaluation. This preserves notification volume exactly.

6. **A suppressed count (`banshee-nvd`).** Every would-be notification that the
   repeat interval swallows increments the episode's `suppressed`. Surfaced as
   "+7 more this hour", it restores intensity to the record without adding a
   single notification. "Quiet" and "muted" stop looking alike.

7. **Inhibition.** A firing episode on a source that
   DOMINATES another mutes the dominated dimension's NOTICE — not its episode.
   Concretely, a kernel-thrash / compressor red inhibits the free-memory
   dimension's notice, because they are the same underlying event and the thrash
   is the more actionable signal. Inhibition is a small static table in core
   (`source → inhibited sources`), applied in the NOTIFICATION step only. Banding
   stays fully independent and honest — the free-memory episode still records; it
   just does not also ring the bell.

8. **Recovering is a sub-state, NOT a seventh Level.** The six Levels
   (`Checking, Quiet, Stirring, Restless, Wailing, Shrieking`) and their glyphs
   are unchanged — ADR-0005 stands, the band still names the glyph. Recovering is
   a modifier the wire carries and the glyph/status may decorate ("was red, now
   clearing"), derived from "there is an open episode on this dimension in its
   `episode_down_secs` window." It never invents a level a client would
   misinterpret.

9. **Pure function over stored state.** `reconcile_episodes(open_episodes,
   pressure, now, config) -> (episode_writes, notifications)` is pure. The sampler
   reads open episodes from the DB, calls it, persists the writes, and delivers
   the notifications. A restarted daemon reads the open episodes back and
   continues them — there is no inter-call RAM state, consistent with
   "hysteresis is a replay, not held state" (ADR-0005, and the gap-in-the-series
   lesson). Episode `startedAt` and `peak` predate the
   ten-minute sample window and therefore MUST live in the DB; the window cannot
   reconstruct a four-hour incident.

10. **Schema v10 is the `alert_episodes` table, and nothing else.** v10 adds the
    table using the same JSON-in-`detail` pattern as `alert_events`, so the
    episode shape can grow WITHOUT a further migration. The thermal dimension
    (v11) and the memory second pass (v13) each got their own forward-only arm.

    **Amended 2026-09-08.** An earlier draft held v10 open as one shared arm for
    all three and kept it off the installed daemon until the other two landed;
    that rule was withdrawn, because every bug that mattered in this project was
    found by running the daemon, not by a fixture, and an additive forward-only
    migration is a Minor change (VERSIONING.md) that costs one migration test.
    The surviving rule: once an arm has run on any installed daemon, append
    nothing to it — bump.

11. **`/alerts` keeps its name; its body becomes `[AlertEpisode]`.** The parity
    contract row `/alerts | banshee alerts | alerts` is unchanged — one thing to
    learn — but the wire type `AlertEvent` is replaced by `AlertEpisode` (newest
    first). Status and the pressure read gain a compact recent-activity summary,
    so every surface, not just `/alerts`, reflects the long memory.

## Consequences

**Easier.** The record now distinguishes 4 storms from 40 and names the peak. An
incident has a beginning, an end, and exactly one "it's over". Flapping produces
one episode. One underlying event produces one notice, not two. `status` and the
menu bar can show "recovering". The thermal dimension and the memory second pass
inherit a migration pattern that is already designed and paid for; deltas and
notifications build on `censusAtPeak` and the episode lifecycle rather than
re-deriving them.

**Harder / given up.** The alert path gains a state machine, and a state machine
is where flapping bugs hide — every transition needs a fixture where the two
plausible readings genuinely come apart (the recurring false-pin shape), and the
daemon must be RUN, not only unit-tested, because the gap-in-the-series and
alert-at-Checking bugs were both invisible to evenly-spaced fixtures. Pre-v10
`alert_events` rows are NOT backfilled into episodes; they remain in the table
for history but are off the new `/alerts` wire (a minor, self-healing regression
— the machine generates new episodes within the hour).

**Escape hatch.** If the episode model proves too heavy, the state machine can
collapse back to today's event model by treating every evaluation's red as a
one-sample episode (`startedAt == endedAt`), which is exactly an `AlertEvent`.
The wire type would carry the extra fields as nulls. Nothing about v10 forecloses
returning to point events.
