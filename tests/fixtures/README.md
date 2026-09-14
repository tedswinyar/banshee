# Shared wire-format fixtures

The exact bytes of the wire format, consumed by THREE test suites so the
format cannot drift between implementations:

- Rust: `rust/banshee-core/src/pressure/tests.rs` and `pressure/headroom/tests.rs` (`include_str!`)
- Swift: `swift/Tests/BansheeCoreTests` (loaded relative to `#filePath`)
- Parity harness: `tests/e2e/run-e2e.sh` (CLI vs raw HTTP vs MCP)

## What is here

| Fixture | Pins |
|---|---|
| `pressure-checking.json` | the no-data verdict: `checking`, empty arrays, 🫧 — **never** `quiet` |
| `pressure-quiet.json` | a calm machine, with `source`, `trendPerSec` and `pending` as EXPLICIT nulls |
| `pressure-shrieking.json` | the FULLY-POPULATED verdict: every nullable field carries a real value, three nested dimensions, three ranked findings — two naming WHO is behind them (`who` + `whoLine`) and one (uptime) naming nobody with an explicit `null`, so both branches are pinned. This is the one that catches a decoder which quietly ignores a field, and the one the encoder key-set tests compare against. |
| `pressure-jetsam.json` | the memory second pass (schema v13): a jetsam kill drives `shrieking` **on its own** — no sustained catastrophic red elsewhere — so it pins the jetsam-immediate rule, not the single-catastrophic-red ceiling. `memory` reads GREEN and `advisory: true` (availability is informational, never a driver); `kernelFree` and `compressor` are advisory reds; only the non-advisory `jetsam` red is a finding. Distinct fixture, not an extension of `pressure-shrieking.json`, whose exact counts are pinned. |
| `headroom-checking.json` | the no-data DECISION (ADR-0010): `shouldWait: true`, parallelism 0, `cores: null`, `Ready: False`, every source `Unknown` — headroom is **never** invented before data exists |
| `headroom-quiet.json` | derived from `pressure-quiet.json` on 8 cores: room for 3 more, `retryAfterSecs` as an EXPLICIT null, a `since` on the two read dimensions and null on the rest |
| `headroom-shrieking.json` | derived from `pressure-shrieking.json` on 8 cores: wait 300s, the reason naming the DOMINANT source (swap), `MemoryPressure: SwapRed` (advisory uptime excluded), every nullable populated somewhere |
| `headroom-throttled.json` | THE co-variance breaker: Restless on 8 cores at 0.20× per core with 12 GB available says "room for 2" by level and cores alone — a FRESH thermal red (45 s) flips it to wait. The Rust suite derives it from the input and both languages decode it |
| `alert-episode.json` | one CLOSED alert episode (ADR-0009): the 2026-09-05 thrash afternoon as a single record — peaked at Shrieking, 39 re-fires absorbed (`suppressed`), the peak's `whoLine` naming who was behind it, and `censusAtPeak` summarising who was running at the worst moment; every nullable populated so an encoder that drops one is caught. An episode recorded before deltas existed has no `censusAtPeak` key — `#[serde(default)]`/`decodeIfPresent` read it back as null, pinned by the "without a peak census" tests in both languages |
| `deltas-normal.json` | the "why did freeing 4 GB not help" story fully populated: a trustworthy verdict at both ends, Chrome freed 4 GB LEADING the consumers by absolute change while claude grew, a census pair present, and a summary that says swap "still" grew. The co-variance breaker — a rank by current size would put Chrome first for the wrong reason |
| `deltas-gap.json` | the honesty case: a 20-minute hole reported at the top AND on every dimension (`observationGapSecs`), `consumers`/`census` the empty branch, and a summary that says the arithmetic "spans a hole" — a gap absorbed silently is the `held_secs` bug all over again |
| `deltas-episode.json` | the episode anchor: `lookback: "episode"`, `episodeId` present (lowercased), compared against the stored `censusAtPeak` rather than a clock interval, so the summary is about the incident |
| `samples.json` | two cheap-tier readings STRADDLING A REBOOT — different `bootTimeSecs` (the delta discriminator), an UPPERCASE UUID (any-case-in), full volume lists |
| `rollups.json` | two 5-minute buckets: one with distinct avg/max and populated deltas, one whose deltas a reboot VOIDED (`null`, which is not zero) |
| `census-full.json` | the FULLY-POPULATED census, ~40 fields across seven nested types: a stale session with `cwd` and a live one with `cwd: null`, a stale-AND-busy tmux session (so the reapable filter is distinguishable from a staleness filter), and per-agent percentages that sum to 46.0 against an honest `monitorTotal` of 29.183 — the never-sum rule made testable |
| `stats.json` | the `/stats` body |
| `datetime-variants.json` | the generous-decode table, cross-checked by both languages |
| `census/*.txt` | raw `ps` / `lsof` / `tmux` bytes, so parser tests start from what the kernel actually printed |

The record payloads carry BOTH pins: the parity harness compares a real
census byte-for-byte across HTTP, CLI and MCP (the cross-SURFACE pin), and the
fixtures above are decoded by both languages' unit suites (the cross-LANGUAGE
pin) — added when the Swift client grew `samples`/`rollups`/`census`/`alerts`/
`stats` and would otherwise have been the only decoder nothing cross-checked.
Names in `census-full.json` are FAKE by construction: the real managed-agent
process names live in the machine-local overlay and must never enter the repo.

Rules (see `../../docs/wire-format.md`):
- keys are camelCase at every nesting depth
- nullable fields are present-as-null, never absent
- datetimes encode as exactly 6 fractional digits with `Z`
- UUIDs encode lowercase; decoders accept any case
- floats are NOT byte-comparable — compare with a tolerance, and never
  re-serialize a payload you are relaying

When a new interop bug is found, add the exact bytes that triggered it as a
new fixture and point all three suites at it.
