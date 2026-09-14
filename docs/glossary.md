# Glossary

Project-specific terminology and coined terms used throughout the documentation.

## Architecture terms

### Constellation
The architectural pattern where one central API hub owns all database access, and multiple thin clients (GUI app, CLI, MCP server) speak HTTP to it. Each client is trivially replaceable; the hub enforces auth and invariants in one place. See: [adr/0001-constellation-architecture.md](adr/0001-constellation-architecture.md)

### Sentinel
Banshee's role: the thing that watches continuously and says *before* the machine falls over, as opposed to the **explorer** that walks the filesystem to say what is taking the space. Banshee reads `statfs` per volume and never walks a directory tree; "what is taking the space" is handed to a disk-usage tool. See: [adr/0003-sentinel-not-explorer.md](adr/0003-sentinel-not-explorer.md)

### Daemon (the LaunchAgent)
`banshee-api`, which owns both the HTTP surface and the sampling loop, installed as a launchd LaunchAgent so sampling outlives a closed window, a quit app, and a reboot. The app is a client of it, not its supervisor; rebuilding does not update it, reinstalling does. See: [adr/0004-collector-as-launchagent.md](adr/0004-collector-as-launchagent.md), [daemon.md](daemon.md)

## Pressure model terms

### Level
The verdict's severity, one of six: Checking, Quiet, Stirring, Restless, Wailing, Shrieking. `Checking` means "not enough data yet" and is deliberately not `Quiet`. At Restless and above the glyph gains the dominant source. Decided once, in core, and served whole. See: [adr/0005-pressure-model-in-core.md](adr/0005-pressure-model-in-core.md)

### Band
A dimension's colour after hysteresis: green, yellow or red. `band` is not a function of `severity` — a dimension recovering from red keeps its red band until it clears the hysteresis margin. Clients branch on `band`.

### Hysteresis
A band changes only when the value crosses the threshold by a margin, and a red band is held until the value has cleared it. Without it a value oscillating around a threshold flaps the glyph; with it a raw value can already have recovered while the band is still red, which is the case several tests exist to distinguish.

### Sustained red / held time
`heldSecs` is how long a band has held, counting back only as far as the observations are CONTINUOUS. A gap in the series (a restart, a laptop wake) ends the run and appears as `observationGapSecs`. This decides whether a red escalates, which is why a gap must never be counted as continuity.

### Advisory dimension
A dimension that produces findings and colours its row but never drives the level or rings a bell — uptime, and the kernel's reclaimable percentage and compressor occupancy in the memory second pass.

### Finding
One ranked line of the worklist: a dimension, its band, a message, an action and its label, and the **who-line** naming the top consumers behind it. Findings arrive ranked by measured impact; clients render them in order.

### Alert episode
One continuous incident on one dimension (ADR-0009): it opens when the dimension has held red for the up delay, stays open through dips shorter than the down window (a re-red inside it REOPENS the same episode, so a flap is one incident), and closes with exactly one recovery notice. Carries its peak and a `suppressed` count — the re-fires it absorbed, so the record can tell one storm from forty. Replaced the point-event alert in schema v10. See: [adr/0009-alert-episodes-and-recovering.md](adr/0009-alert-episodes-and-recovering.md)

### Recovering
A modifier on the verdict, not a seventh level: some dimension has dropped below red inside its open episode's down window. The band is honest about now; `recovering` is honest about the recent past. Core decorates a calm glyph with 🩹 for it.

### Headroom
The verdict reduced to a decision for agents: `shouldWait`, `recommendedParallelism`, `retryAfterSecs`, and a set of Kubernetes-style conditions. Advice, never enforcement. See: [adr/0010-headroom-is-a-decision.md](adr/0010-headroom-is-a-decision.md)

### Deltas
What changed over a lookback, or since an alert episode opened: per-dimension then/now/delta, consumers ranked by change rather than size, and an explicit observation gap when the arithmetic spans a hole.

### Census
The five-minute tier: agent sessions, IDE-spawned helpers, orphaned helper processes, tmux sessions, app process groups and monitoring agents, from two `ps` calls, `tmux`, and `lsof` only for sessions already flagged stale. See: [signal-collection.md](signal-collection.md)

## Wire format and testing terms

### The Five Rules (wire format)
The exact wire format contract enforced between Rust and Swift:
1. camelCase keys at every depth
2. Nullable fields present-as-null (never omitted)
3. Datetimes encode 6-digit-microsecond Z form (`2026-08-19T14:23:00.123456Z`), decode generously
4. UUIDs lowercase out, any case in
5. Unknown fields rejected (`deny_unknown_fields`) on write DTOs

See: [wire-format.md](wire-format.md)

### Parity harness
`tests/e2e/run-e2e.sh` — the black-box gate that reads the same record through the CLI, raw HTTP, and the MCP server and requires the three views to match byte-for-byte. It uses no internal knowledge and is what enforces the wire-format contract here.

### Forward-only migration
The database versioning policy where migrations only go forward in version, never backward. The schema has a `user_version` pragma; if the app finds a newer version than it knows, it refuses to open the DB rather than risk corruption. Rolling back versions requires restoring from a backup. See: [data-safety.md](data-safety.md)

### Mutation-proof
A test is mutation-proof when deliberately breaking the production code (e.g., removing an `ORDER BY` clause or inverting a condition) causes that specific test to fail. If a mutation goes undetected, it usually means the test is mocking the wrong layer or not asserting the actual behavior. `scripts/mutate.sh` applies a mutation and refuses to report a green run it cannot vouch for.

### Mock at boundaries
The testing principle: drive real parsers, validators, and mappers; mock only at the network boundary (`MockAPIClient` in Swift). A test that mocks the layer it claims to verify passes while proving nothing.

### False pin (co-varying fixture)
A test that passes identically before and after the fix it claims to pin, usually because the fixture makes the thing under test indistinguishable from something else (two properties that move together). Ask of every pin: what other implementation would also pass this? See: [lessons/](lessons/)

## Profile and configuration terms

### Profile (prod/dev/test)
The runtime mode selected by `BANSHEE_PROFILE`. Each has its own database path, port, and safety rules. Test profile refuses any DB path under the prod data directory — a mis-set env var cannot touch real data. Profiles are NOT "performance profile" or "user profile"; they're environment configurations. See: `rust/banshee-api/src/config.rs`

### Port ladder
When the configured port is busy, the API server tries port+1, port+2, up to port+9, then gives up. The server announces the ACTUAL bound port on stdout (`banshee-api listening on http://127.0.0.1:<port>`). Configured ports are requests, not facts. Prod pins its port and does not climb. See: [daemon.md](daemon.md)

### P1–P7, WS1–WS5
Shorthand in commit messages, gotchas and mutation notes for the project's build-out phases (P1–P7, August 2026: the daemon, the pressure model, the menu bar, the detail window, actions) and the 1.0 workstreams (WS1 dimensions, WS2 attribution and deltas, WS3 headroom for agents, WS4 alert sinks, WS5 release posture). They date a lesson; the bead ID beside them is the durable reference.

### Machine-local overlay
`census.local.toml`, read from outside the repository, holding that machine's managed-agent process names, app groups and build-tool busy patterns. Kept out of the repo by construction, not by `.gitignore`. See: [signal-collection.md](signal-collection.md) → Configuration
