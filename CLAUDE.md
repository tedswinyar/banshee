# Banshee — Agent Guide

Read this before touching code. `AGENTS.md` covers issue tracking (beads) and
session completion; this file covers the system, the gates, and the standards.

| For | Read |
|---|---|
| Issue tracking, landing the plane | `AGENTS.md` |
| Human tour (clone → app in 10 min) | `docs/getting-started.md` |
| Adding a field end-to-end (the touch-point checklist) | `docs/adding-a-field.md` |
| Release/DMG pipeline and its failure modes | `docs/build-pipeline.md` |
| **Running Banshee as a daemon** (install, update, diagnose) | `docs/daemon.md` |
| SQLite backup/migration conventions | `docs/data-safety.md` |
| Versioning policy | `VERSIONING.md` |
| Wire format (the exact bytes) | `docs/wire-format.md` |
| **What Banshee measures, and how** (the `perf-scan` port spec) | `docs/signal-collection.md` |
| **Why disk hotspots are out of scope** (a dedicated disk tool's job) | `docs/adr/0003-sentinel-not-explorer.md` |
| Project terminology (constellation, sentinel, etc.) | `docs/glossary.md` |
| Troubleshooting (build/runtime/test failures) | `docs/troubleshooting.md` |
| Where the private material lives (notebook, gate, fresh-history rule) | `AGENTS.md` → One Repo, One Notebook; `docs/adr/0011-one-public-repo-nested-notebook.md` |
| Template lineage & backporting improvements | `TEMPLATE.md` + `.template-stamp.toml`, in the maintainer's notebook (`private/`) |

> **Shape conventions inherited from the project template:** camelCase wire types
> with explicit nulls, fixtures shared by both languages, and one CLI subcommand plus
> one MCP tool for every HTTP read. Nothing else of the template's demonstration
> slice remains (its `Note` table was dropped by schema v6); a `// TEMPLATE SLICE`
> marker or a `Note` reference outside migration history is a leftover — remove it.

## Architecture

A constellation: one Rust API hub owns the database; every client speaks HTTP
to it. Nothing else opens the DB — one writer, one place for auth and
invariants. See [docs/adr/0001-constellation-architecture.md](docs/adr/0001-constellation-architecture.md) for the full diagram and decision rationale.

| Component | Path | Role |
|---|---|---|
| `banshee-core` | `rust/banshee-core/` | Domain types, SQLite store, schema migrations, verified backups, **and the whole pressure model** — bands, hysteresis, levels, glyphs, findings (ADR-0005), the alert-episode lifecycle (ADR-0009), and the headroom decision for agents (ADR-0010). No HTTP. Also owns `DEFAULT_API_PORT`, so no client can spell the port differently. |
| `banshee-api` | `rust/banshee-api/` | Axum server: `/health` (open) + keyed reads, plus a few authenticated POST action routes (reap preview/execute, backup). The sampler is the only writer of the time series — no create/update/delete of stored readings. |
| `banshee-cli` | `rust/banshee-cli/` | `banshee` binary. Scripting-friendly: `--json`, stable exit codes (0/1/2/3/4 — see `src/main.rs` header). |
| `banshee-mcp` | `rust/banshee-mcp/` | MCP stdio server (hand-rolled JSON-RPC, house pattern). Talks HTTP to the API. |
| `Banshee` | `swift/Sources/Banshee/` | SwiftUI macOS app. A **client** of the daemon — it does NOT supervise or start it (ADR-0004). |
| `BansheeCore` | `swift/Sources/BansheeCore/` | APIClient (the ONE network boundary) over the daemon's Unix socket via `UnixSocketHTTP` (ADR-0008; `URLSession` cannot), wire models, `PressureModel` + `DetailModel`, `WireDate`, `DaemonService`. At parity with the HTTP surface since P6b: all nine reads (headroom since WS3a, deltas since WS2d), cross-language-pinned against the shared fixtures. |
| `DesignKit` | `swift/Sources/DesignKit/` | Design tokens. Views never hardcode spacing/colors (see `skills/swift-conventions.md`). |

**Wire format is a contract, not a convention.** camelCase keys at every
depth; nullable fields present-as-null; datetimes encode 6-digit-microsecond
`Z` form, decode generously; UUIDs lowercase out, any case in. It is
implemented twice (`rust/banshee-core/src/wire_time.rs`,
`swift/Sources/BansheeCore/WireDate.swift`) and pinned by SHARED fixtures in
`tests/fixtures/` that both unit suites and the e2e harness consume. Change
the wire format → update the fixtures → both languages' tests tell you what
broke.

## Data, ports, profiles

Selected by `BANSHEE_PROFILE` (default `prod`). Resolution logic and tests:
`rust/banshee-api/src/config.rs`.

| Profile | Database | Port | Key file |
|---|---|---|---|
| `prod` | `~/Library/Application Support/banshee/banshee.db` | 18769 | `~/Library/Application Support/banshee/api_key` |
| `dev` | `…/banshee-dev.db` | 18779 | same as prod |
| `test` | `BANSHEE_DB_PATH` (required) | `BANSHEE_PORT` (required; 0 = ephemeral) | `BANSHEE_KEY_FILE` (required) |

- **Test mode wins**: the test profile refuses any DB path under the prod data
  dir. A mis-set env var cannot touch real data.
- **One env convention for every client**: `BANSHEE_API_URL`,
  `BANSHEE_API_KEY`, `BANSHEE_KEY_FILE` are read identically by the CLI,
  the MCP server, and the Swift app.
- The API climbs a port ladder (configured port + 0..9) when its port is busy
  and **announces the bound port on stdout**:
  `banshee-api listening on http://127.0.0.1:<port>`. The configured port
  is a request; only the announcement is the truth.

## Build & Test

```bash
./scripts/verify.sh            # THE gate: every suite, layer auto-detected
./scripts/verify.sh --rust-only|--swift-only|--scripts-only
make build                     # debug build, Rust + Swift
make app                       # assemble build/Banshee.app (ad-hoc signed)
make daemon-install            # install/UPDATE the LaunchAgent — see below
make daemon-status             # the daemon's machine state (NOT part of verify)
./scripts/start.sh             # dev-profile API in the foreground
./scripts/warm-dev-db.sh       # let the daemon gather enough history to show a verdict
./scripts/dev-build.sh         # debug app + seed + launch
```

Suites and where they live:

| Suite | Location | Notes |
|---|---|---|
| Rust unit | `#[cfg(test)]` in each crate | parsers start from raw fixture bytes |
| Rust integration | `rust/banshee-api/tests/` | boots the real router over HTTP on a temp DB |
| Swift unit | `swift/Tests/` | `MockAPIClient` is the ONE approved mock (network boundary) |
| e2e parity | `tests/e2e/run-e2e.sh` | **the parity table**: every read must exist on HTTP + CLI + MCP, and their bodies must match BYTE-FOR-BYTE (unnormalized) |
| Scripts suite | `scripts/tests/test-*.sh` | tests OF the gates: hooks, verify layers, the residue gate, the release-repo resolver, **the daemon install/check** (against a temp dir, never the real machine) |

> **REBUILDING DOES NOT UPDATE THE RUNNING DAEMON.** `cargo build` writes to
> `rust/target/`; the LaunchAgent runs an installed copy in
> `~/Library/Application Support/banshee/bin/` and never looks at `target/`
> (deliberately — ADR-0004; a plist pointing into `target/` cost a sibling project
> an eight-day silent outage). `make daemon-install` is idempotent and is the way
> to ship any change to the daemon. `make daemon-status` says what is actually
> running.

The pre-push hook runs `check-hooks.sh` then `verify.sh` on every branch push
(`scripts/install-hooks.sh` installs it; the gate body is compared
byte-for-byte against the canonical block, because a marker alone proves
nothing).

## Reading a verify run

One line per suite: `── rust  PASS (11s, log: .runtime/verify/rust.log)`.
On FAIL you get the last 15 log lines inline; the full log is in
`.runtime/verify/<suite>.log`. `skipped (layer not present)` is normal in
pruned projects — verify auto-detects layers by directory presence.

## Gotchas

Each item here must cite what it cost — this section is EARNED, not written.
Seeded from the upstream template's lineage (sibling projects, verified on this machine
class); add your own the moment one bites.

- **`verify.sh` exit 75 is a lock, not a failure.** Another verifier holds the
  per-working-copy lock. Wait and retry. Do not delete the lock dir, do not
  conclude the suite is flaky. (Two concurrent Swift builds in one checkout
  corrupt each other's build state — that is why the lock exists.)
- **The API server's stderr goes to a log file, not the UI.**
  `~/Library/Logs/banshee-api.err.log` — launchd redirects it there
  (`scripts/lib/service.sh` owns the path). A
  `tracing::error!` is visible ONLY there. If a user or operator must see it,
  it has to travel over the API — a response, a status field, a stored record.
  (Predecessor project piped stderr to a handler that only ran at exit;
  running-server logs reached nobody.)
- **`bd hooks install`, run bare, breaks pushes.** It sets
  `core.hooksPath=.beads/hooks`, which shadows every hook already in `.git/hooks`
  and turns a directory `bd init` auto-commits into a hook directory that other
  tooling on the machine may write into — a non-executable file there fails every
  push with `exec format error`, and whatever lands there gets committed. Follow
  the hooks pattern in `AGENTS.md` (temporary hooksPath dance). If pushes fail
  with "cannot execute binary file", start there.
- **SourceKit lies before the first build.** Fresh checkouts and new files
  show "No such module" / "Cannot find type" diagnostics that a plain
  `swift build` resolves. Build first; trust the compiler, not the editor
  overlay.
- **Never index an array unguarded inside a test assertion.** Assert the whole
  array in one comparison, or `XCTUnwrap`. A count assertion is NOT a guard —
  `XCTAssertEqual(x.count, 3)` records a failure and then execution continues
  into `x[2]` and traps. Related: `XCTSkipIf` reports SKIPPED, which keeps
  verify green — a violated invariant must FAIL, not skip. (Predecessor
  recorded `Executed 51 tests, with 0 failures` next to `** TEST FAILED **`.)
- **A pin that only passes because of a BUILD DEFAULT proves nothing (2026-08-31,
  P1).** A cascade test asserted that `PRAGMA foreign_keys = ON` was doing its
  job. It passed with the pragma deleted, because `rusqlite`'s **`bundled`
  SQLite is compiled with `SQLITE_DEFAULT_FOREIGN_KEYS`** (confirmed via
  `PRAGMA compile_options`) — enforcement was already on and the test could not
  distinguish. Fixed by starting each such test from the hostile state
  (`PRAGMA foreign_keys = OFF`) and proving `validate_or_init` restores it. Cost:
  a test that looked like safety and was decoration, caught only because the
  mutation was actually applied. **The pragma is still required** — swapping
  `bundled` for a system SQLite silently returns to FK-off, and then the
  retention sweep leaks orphaned child rows forever while looking healthy.
- **THE recurring false-pin shape: a fixture where two properties co-vary
  (four occurrences, P1–P3).** Every false pin in this project has had the same
  cause — the test data made the thing under test indistinguishable from
  something else. (1) A foreign-key cascade test passed with its pragma deleted,
  because the bundled SQLite already defaults it on. (2) A monitor-agent test
  could not tell `max(lifetime)` from `sum(lifetime)`, because the group had one
  process and the two are equal. (3) A dedup test's fixture had no overlapping
  match. (4) A "band beats severity" test gave the worse band the higher severity
  too, so comparing either first gave the same answer.
  A fifth instance of the same question, found by RUNNING the daemon rather than
  by a test: alerts fired at `Level::Checking`, because every alert test used a
  trustworthy level and so never composed "verdict not yet trustworthy" with "this
  dimension is red". The alerts could never be delivered but still burned the
  per-dimension cooldown, silently eating the first hour of real alerting after
  every start. **Unit tests verify the cases you thought of; running it finds the
  ones you did not.**
  **Use `scripts/mutate.sh`** — it refuses to report a green run it cannot vouch
  for (exit 3 if the target text is absent, exit 2 if no test ran).
  **Ask of every pin: what OTHER implementation would also pass this?** Then make
  the fixture distinguish them — usually by finding the case where the two
  properties genuinely come apart (for (4), a hysteresis-held band whose raw value
  has already recovered).

  **The gap fix (`banshee-s6s`) produced FIVE more in one sitting**, all of the same
  shape and all caught by `scripts/mutate.sh` rather than by inspection:
  (6) a mean-vs-median fixture where the mean also caught the outlier — a mean only
  fails when the gap is a large fraction of the window, so the fixture had to be
  SMALLER, not more dramatic; (7) a per-tier-ceiling test using five censuses, which
  self-calibrated and so passed with the per-tier logic removed — the configuration
  is only load-bearing when the series is too short to calibrate; (8) a
  configured-floor test with a UNIFORM sub-second series, where every interval was 0
  and `0 > 0` is false, so a zero ceiling behaved identically to a 3-second one;
  (9) a cadence-propagation test that asserted on a hand-built config and never
  called `from_env`, the function that could actually break; (10) a tolerance that
  was configurable in `PressureConfig` and hardcoded as `3` in `Series`, so retuning
  it did nothing to any series long enough to self-calibrate. **A config value that
  silently does nothing is worse than no config value.**
- **A mutation that fails to APPLY is indistinguishable from a mutation that was
  CAUGHT (2026-08-31, P2).** Mutation-testing by string-replacing source text
  silently does nothing when the target text has moved — and `cargo fmt` moves it,
  because it reflows multi-line closures and argument lists. A dedup mutation
  "passed" three times before the source was inspected and found unmodified. The
  test was fine; the harness was lying. Now handled by **`scripts/mutate.sh`**,
  which exits 3 when the target text is absent and 2 when no test ran, so it can
  never report a green run it cannot vouch for. Copy the target text out of the
  CURRENT file, after `cargo fmt`.

  That script then reproduced the same class of bug on itself: with
  `set -o pipefail`, `cargo test | grep -q` returns cargo's exit 101 under a
  *caught* mutation, so a real pin was reported as false. It now captures to a
  file and greps that. **A checker that can fail silently needs its own
  self-test** — `scripts/mutate.sh`'s header documents the four it has.
- **A mutation harness must cover EVERY suite, or it silently stops covering the
  ones it missed.** `mutate.sh` handled only `cargo` until P4 (Swift added) and P5
  (the scripts suite added). Both gaps were found the same way: reaching for the
  harness on a new layer and discovering it could not run there, then hand-rolling
  a runner and getting it wrong. The shell layer matters most of the three — it is
  where the daemon install/check gates live, and they guard an eight-day silent
  outage shape.
- **A mutation checker must ask "did a test RUN?" before "was there an error?"
  (2026-08-31, P4).** An XCTest assertion failure prints
  `PressureModelTests.swift:44: error: -[…] : XCTAssertEqual failed`, so a
  hand-rolled Swift mutation runner that grepped for `error:` classified a
  perfectly caught mutation as a broken one — and "broken mutation" is
  indistinguishable from "caught mutation" in exactly the way the harness exists
  to prevent. `scripts/mutate.sh` now dispatches on the file path (`swift/…` →
  `swift test`, otherwise `cargo test`) and decides in order: did any test run,
  then did any fail. **Mutation-test the Swift layer through the same script**;
  a second hand-rolled runner is how the Swift pins quietly stop being checked.
- **The recurring false-pin shape has a FIFTH instance, and it is instructive
  about `decide` (2026-08-31, P4).** `Pressure::checking` builds its verdict from
  `decide(&[], 0, false)` so it cannot name a level itself, and the obvious
  mutation — flipping that `false` to `true` — proves nothing: `decide` returns
  `Checking` for an empty contribution slice either way, because the
  `scoring.is_empty()` guard runs before `have_data` is consulted. The mutation
  that bites is the one that hardcodes `Level::Quiet` in the struct literal,
  because naming a level is the mistake the function exists to prevent. **Mutate
  the thing the test CLAIMS, not the nearest line to it.**
- **`rusqlite` refuses a `u64` above `i64::MAX`, and it fails the WHOLE INSERT**
  (`TryFromIntError(PosOverflow)`). SQLite has no unsigned integer type. For a
  sampling daemon that is the wrong failure: one absurd reading would stop the
  time series rather than blemish one field. Every `u64` column goes through
  `clamp_i64` in `sample_store.rs`. Found by a test, not in production.
- **`Retention.raw` has a hard floor of one rollup bucket.** The sweep cutoff is
  `bucket_start_of(now - raw)`, so any `raw` below `ROLLUP_BUCKET_SECS` behaves
  identically to one bucket — the current, still-open bucket is never rolled up
  or deleted. Deliberate (summarising a bucket that can still gain samples
  records a partial average as final), but it means a sub-bucket value is
  silently not honoured. A test that tried to force a sweep by setting `raw` to
  1ms failed for this correct reason; pre-seed genuinely aged data instead.
- **SQLite does not return freed pages to the OS.** After a retention sweep the
  file does NOT shrink: deleted pages are marked free and reused, and only an
  explicit `VACUUM` (whole-file rewrite, write lock) reclaims them. So the
  on-disk size settles at the **high-water mark**, not at the current row count.
  ADR-0006 promises a bounded row count; "the file shrinks" is a different and
  false claim. Measured steady state: ~2.4 MB raw (24h) + ~6.0 MB rollups (30d).
- **Batch reads need an index, not a scan (626 ms → 20 ms).** `attach_volumes`
  originally did `samples.iter_mut().find(|s| s.id.to_string() == sample_id)` per
  volume row — O(N²) *with a `String` allocation per probe*. Over a 24-hour
  window (5,760 samples) that was 626 ms. A `HashMap<Uuid, usize>` built once
  takes it to ~20 ms. Also: the `IN (...)` list is chunked at 500, because
  `SQLITE_MAX_VARIABLE_NUMBER` is 32,766 on current builds but **999 on older
  ones**, and a day is 5,760 ids.
- **`serde_json` does not round-trip `f64` bit-exactly (2026-08-31, P3).**
  `1.1574074074074077e-5` serialises correctly and parses back one ULP lower;
  reproduced in a standalone crate, so it is the library, not our code. **Never
  compare a wire float with `==`** — in production or in a test. A `Pressure`
  round-trip test asserted exact equality and failed for this reason; it now
  compares floats with a relative epsilon and everything discrete exactly. Same
  class as the documented datetime tolerance; recorded in `docs/wire-format.md`.
- **`pages_free` is not availability, and banding on it made the memory dimension
  a false-positive generator (2026-09-01, `banshee-cot`).** macOS keeps the free
  list deliberately tiny — a small free target, with everything reclaimable parked
  in the speculative/purgeable/file-cache pools — so a healthy machine reads
  100–500 MB "free" forever. Measured: Banshee red at 84 MB "free" at the same
  instant `kern.memorystatus_vm_pressure_level` said NORMAL (1) and
  `memory_pressure` reported 56% free (~9 GB reclaimable); six overnight memory
  alerts on a machine the kernel considered fine. The port spec even named
  `memory_pressure`'s free percentage as the source — the implementation drifted
  to `vm_stat`'s free list. Availability is `free + speculative + purgeable +
  external` (`SysReading::available_bytes`), and no test caught it because every
  memory fixture co-varied free with available. The kernel's own pressure level
  is the arbiter when in doubt: it is the one signal that cannot disagree with
  the kernel. Cost: a night of alert-log noise and the user's first "is this
  thing lying to me?" — the exact currency a monitor cannot spend.
- **Never sum a list of per-group percentages.** The census's `monitor_agents`
  rows each divide cumulative CPU by the longest-lived process *in their own
  group*, so the denominators differ and adding them is meaningless — a real
  census summed to 59.9% of one core against an honest 49.2%, a 10.7-point
  overstatement. Overlapping patterns double-count on top of that. Use
  `Census::monitor_total`, which counts each process once against one global
  denominator. Same shape as the `%CPU`-vs-cumulative-time trap: a number that
  looks like an answer and is not.
- **A build directory is not an installation, and the failure is invisible for
  days (ADR-0004).** A sibling project's LaunchAgent plist named
  `rust/target/release/<its>-api`; a benchmark run recreated
  `target/release` without that binary, and the running process kept serving from
  its **deleted inode for eight days** until a reboot exposed it. `launchd` then
  reported exit **78** — which is `EX_CONFIG`, "cannot exec the program", NOT "your
  plist is malformed" — and two more days went into that misreading. Banshee
  installs to `~/Library/Application Support/banshee/bin/` via an atomic
  `mv` of a `.incoming` copy, and `scripts/check-service.sh` refuses any plist
  whose `ProgramArguments[0]` contains `target/`.
- **Current `launchctl print` prints `EX_CONFIG`, not `78` (verified 2026-08-31,
  macOS 15).** A check keyed only on the number is dead code on this machine.
  `check-service.sh` matches both spellings — found by deleting a live agent's
  binary and reading what launchd actually said, not by reading documentation.
- **Read a plist with `PlistBuddy`, never `grep`.** launchd REWRITES plists in
  binary form once it has loaded them, so a `grep` that worked on the XML you just
  wrote silently finds nothing an hour later — and "nothing" reads as "the key is
  absent" rather than "I cannot read this file".
- **`launchctl bootstrap`/`bootout`/`kickstart`, never `load`/`unload`.** The legacy
  verbs are deprecated and fail quietly: `launchctl load` can return 0 on a plist
  launchd then declines to run, which is indistinguishable from success until you
  notice nothing is sampling. `bootout` is also ASYNCHRONOUS — poll
  `launchctl print` for it to disappear rather than assuming it is done.
- **`${VAR:-default}` substitutes for EMPTY as well as unset (2026-08-31, P5).** A
  test helper took `port="${3:-$DEFAULT}"` and was called with `""` to build an
  unpinned-port fixture; it silently wrote the default instead, so the
  "unpinned port is reported" test passed while testing nothing. Use `${3-default}`
  when an explicitly empty argument is meaningful. Same family as the co-varying
  fixture: the test could not distinguish the case it was named for.
- **A GAP in the series is not evidence of continuity, and `held_secs` decides
  severity (2026-09-01, `banshee-s6s`).** `Series::held_secs` measured
  `newest - first_of_run` from real timestamps — correct as far as it went, since the
  two tiers tick at different rates — and so counted straight across holes.
  `held_secs` feeds `is_sustained_red`, which escalates Restless → Wailing →
  Shrieking, so **any gap longer than the 120s grace made the first red reading
  after it look sustained.** Observed minutes after the daemon was first installed:
  a 17-minute hole (an earlier process, then a restart) produced `held 17m` on every
  dimension off 45 seconds of observation, and a false 💀 with two alerts recorded.
  The trigger is mundane and DESIGNED IN — every `make daemon-install`, and every
  laptop wake, because `sampler::run` uses `MissedTickBehavior::Delay` specifically
  so a sleeping machine leaves a gap. **Found by running the daemon, not by a test**,
  and the reason no test saw it is the recurring one: every `held_secs` fixture used
  an EVENLY SPACED series, where "counts the span" and "counts continuous
  observation" give the same answer.
- **An ad-hoc `python3` string-replace fails SILENTLY, exactly like the mutation
  harness used to (2026-09-01).** A rewrite of
  `the_census_tiers_normal_spacing_is_not_a_gap` did nothing, because `cargo fmt`
  had already reflowed the target text — and the test then kept passing under a
  mutation it was written to catch, which is the only reason it was noticed. This is
  failure mode (1) from the `mutate.sh` header, in a different tool. **Put an
  `assert old in s` in every scripted edit**; `mutate.sh` exits 3 for this reason and
  hand-rolled edits deserve the same floor.
- **`Contents/MacOS/` is CASE-INSENSITIVE, and an app bundle can silently contain
  the wrong program (2026-08-31, P6).** `build-app.sh` copied the Rust binaries into
  `Contents/MacOS/` so `Bundle.url(forAuxiliaryExecutable:)` could find them — and on
  the default macOS filesystem `MacOS/Banshee` (the app) and `MacOS/banshee` (the CLI)
  are THE SAME FILE, so the CLI overwrote the app. `open Banshee.app` then launched
  the CLI, which printed usage to a terminal nobody was watching and exited 0: no
  crash, no crash log, no Dock icon, nothing in `pgrep`. **The bundle had never been
  launchable**, and nothing noticed because until P6 it was only ever built, never
  run. Helpers now live in `Contents/Helpers/`, `build-app.sh` refuses to finish if
  the main executable does not link SwiftUI, and `scripts/tests/test-app-bundle.sh`
  guards both. It is a TEMPLATE bug — any stamp of the upstream template whose app and CLI names
  differ only by case has it.
- **`MenuBarExtra` cannot render an emoji label, and fails INVISIBLY (2026-08-31,
  P6).** `MenuBarExtra { } label: { Text("💀🧠") }` produced a status item that
  existed in the accessibility tree with the right `AXTitle` — and drew nothing a
  person could see. SwiftUI turns a MenuBarExtra label into an image, and the emoji
  did not survive it. Switching to `NSStatusItem` and `button.title` was no better:
  a status item draws its title as a MASK, so 💀🧠 became a single hollow ring.
  **The working shape is `button.image` from an `NSImage` with
  `isTemplate = false`** (`DesignKit.MenuBarGlyph`); a template image is drawn as a
  silhouette so it can be tinted, which erases the colour that IS the information —
  😴 and 💀 are the same shape in a mask. A sibling project reached the same conclusion
  independently (its status bar controller: "Uses NSStatusItem
  + NSPopover (AppKit) instead of SwiftUI's MenuBarExtra"). **No test can see any of
  this**: the app runs, the item exists, the AX label is correct, and the menu bar
  looks empty. The only way to know is to screenshot the menu bar and look — which is
  why the pixel check is worth the trouble, and why `isTemplate` now has a pin.
- **Do not test a GUI binary by running it.** The first version of that bundle test
  asserted `Banshee --help` does not print CLI usage — which LAUNCHES the app, which
  never exits, and hung the scripts suite until it was killed. `otool -L` and `nm`
  answer "which program is this" without starting anything.
- **`log` is a zsh BUILTIN in this shell, and it prints nothing (2026-09-07).**
  `log show --predicate 'subsystem == "com.tedswinyar.banshee"'` run bare returned
  empty, which read as "the app logged nothing" for twenty minutes while the app was
  logging steadily. Always `/usr/bin/log`. There is no error to notice: the builtin
  reports watch/logout events and exits 0.
- **`cp` is interactive in this shell** (aliased to `cp -i`). A background job
  that copies over an existing file hangs forever on an unanswerable `overwrite?`
  prompt. Use `/bin/cp -f` in any scripted or backgrounded work. Cost: one
  wedged mutation-testing run that had to be killed mid-sequence, with a mutated
  source file left on disk.
- **A shape regex for bead IDs mangles NAMES, and the damage reads as intent
  (2026-09-05/06, `banshee-2yl`).** `banshee-[a-z0-9]{3,4}` matches `banshee-api`,
  `banshee-dev`, `banshee-key`… A hand scrub of the public mirror turned the
  `banshee-daemon:` message prefix into `on:`, `/tmp/banshee-service-test` into
  `/tmp/ice-test`, and a test fixture name into `"-\(UUID…"` — all committed, none
  noticed, because each result is a plausible identifier. The mirror that did this is
  retired (ADR-0011) and bead IDs now ship as the harmless tokens they are, but the
  lesson stands for ANY scripted rewrite: strip only names that EXIST on a list, never
  a shape, and gate the shape the other way round.
- **A MOVED checkout with warm build dirs fails in ways that look like code bugs
  (2026-09-07, `banshee-xoo`).** Moving a checkout (then the public mirror's) out of /tmp with
  its `rust/target/` and `swift/.build/` intact made the public verify fail on two
  suites: SwiftPM had the Sparkle xcframework's ABSOLUTE old path in `.build/`, and
  the CLI exit-code tests spawned `env!("CARGO_BIN_EXE_banshee")` — a path baked in
  at compile time under `/private/tmp/…` that cargo does not refingerprint when the
  tree moves, so the test binary was considered fresh and spawned a file that no
  longer existed (`NotFound`). Neither message names the move. After relocating any
  checkout, delete both build dirs before trusting a green or a red. Cost: one failed
  run and a full rebuild.
- **A job-level env var is inherited by every test the job runs, and it can flip the
  suite's mode without a word (2026-09-14, first pipeline rehearsal).** `release.yml`
  exports `BANSHEE_DRILL_SELF_CONTAINED=1` for the whole job; `release.sh` runs verify,
  verify runs `test-release-checklist.sh`, and its seven stubbed "the drill FAILS when…"
  cases inherited the flag, went self-contained, booted a real daemon and passed —
  "succeeded and should not have", on a suite that was green in CI four minutes earlier.
  Same runner, same commit, one env var. The test now `unset`s the drill's mode
  variables at the top. **A test that drives a script by env must start from a clean
  env**, not the caller's; `env -u` per case is not enough when a new variable arrives.
- **A gate that runs BEFORE a step cannot see what that step commits, and `--no-verify`
  makes sure nothing else does (2026-09-20, `banshee-282`).** `release.sh` runs verify,
  THEN generates `THIRD-PARTY-NOTICES.html` and `sbom/*.cdx.json` and commits them, and
  `release.yml` pushes main `--no-verify` (verify had "just run"). The first end-to-end
  release landed 311 third-party author e-mail addresses on public main, and the next
  verify on the dev machine went red on the residue gate's real-tree check — found only because C3
  called for a fresh-clone gate run. The gate now masks addresses in exactly those two
  generated files (a home path in a `path+file:///Users/…` purl is still refused — the
  SBOM embeds the build user's), and `release.sh` re-gates the tree after the commit.
  **Every gate must run after the last write it is meant to catch.**
- **macOS file permissions, verified by execution (2026-08-06):** `chmod 600`
  does not remove an ACL (`chmod -N` does); `ls -l` cannot show you whether an
  ACL exists (use `ls -le`); `/bin/chmod 600 -- f` exits 1 *while still
  applying the mode*; unlinking needs write on the DIRECTORY while truncating
  needs it on the FILE — an 0600 file you own can always be emptied even when
  it cannot be removed.

## Conventions

- **Commits:** Conventional-Commits-ish (`feat:`, `fix:`, `test:`, `chore:`,
  `docs:`, `bd:`, scoped forms fine). Bodies matter: state what was proven and
  how (before/after output, which mutation was applied and which test caught
  it).
- **Bead IDs** appear in commit subjects when work traces to an issue:
  `fix: … (banshee-1234)`. Never markdown TODO lists — see `AGENTS.md`.
- **Wire types go through `Wire.decoder()`/`Wire.encoder()`** (Swift) and
  `wire_time` (Rust). An ad-hoc `JSONDecoder()` for a wire type is a review
  finding — date handling silently diverges.
- **Design tokens only** in SwiftUI views: `Spacing.*`, `Palette.*`,
  `Typography.*`, `Radius.*`. Raw point values and color literals are review
  findings (`skills/swift-conventions.md`).

## Testing standard

**Coverage percentage is REJECTED as a test target.** The bar is structural:

- **Mutation-proof.** Make 2+ deliberate single-line production mutations per
  file under test and report which test caught each, using `scripts/mutate.sh`
  (it dispatches to `cargo test`, `swift test`, or a scripts-suite test by file
  path). An uncaught mutation usually means a missing seam, not a missing
  assertion. (Worked example: hardcode `Level::Quiet` in `Pressure::checking` and
  `the_checking_placeholder_is_what_the_model_says_about_no_data` fails — which is
  the guard on the one lie the menu bar must never tell.)
  **Then record it in `.mutations/`** so it stays proven: a pin verified once and
  written only into a commit message rots silently when a refactor moves the
  target text (`cargo fmt` alone will). Each `.mutations/*.mut` names the exact
  text, the mutation, and the catching test; `make mutations`
  (`scripts/replay-mutations.sh`) replays them all and fails if any no longer
  APPLIES or is no longer CAUGHT. It is NOT in the pre-push gate (every entry
  compiles + tests — minutes); run it before a release and quarterly. The
  scripts suite's `test-mutations-manifest.sh` keeps the manifest well-formed on
  every run (banshee-jds).
- **Mock ONLY at boundaries.** Drive the real parsers, validators, mappers;
  mock the network (`MockAPIClient`). A test that mocks the layer it claims to
  verify passes while proving nothing.
- **Parser tests start from RAW BYTES** (`tests/fixtures/*.json`), not from an
  already-decoded value round-tripped through our own encoder.
- **Every error branch is reached.**
- **A "pin" that passes identically before and after a fix is not a pin.**
  Apply the reverting mutation and watch the test fail before trusting it.

## Code health: track the ratio, do not budget a percentage

No percentage budget for code-health work — percentages are Goodhart-able and
schedule health as a project, which then decays. **Track the fix:feat commit
ratio** instead:

```bash
git log --format=%s | grep -cE '^fix(\(.*\))?: '
git log --format=%s | grep -cE '^feat(\(.*\))?: '
```

Measure your own baseline at week 8 of the project and re-measure quarterly.
A `fix:` commit is a `test:` commit you did not write, priced at the cost of
finding it in production.

# Sharpening Review Team

This project uses the Sharpening methodology for code and design review. When
an agent team is created, follow these principles.

## Core Principles

- **Rule of Five**: Every review must go through multiple iterative passes before convergence
- **Convergence**: Work is not done until the reviewer declares "this is as good as it can be"
- **Adversarial Collaboration**: Teammates must actively challenge each other's findings — don't just agree

## Team Roles & Responsibilities

### Technical Reviewer
You own correctness. Your job is to find bugs, type errors, missing error handling, and logic flaws. You are the first line of defense. If something is technically wrong, you catch it. Challenge the Architect if their proposed patterns introduce complexity that could harbor bugs. Challenge the Quality Auditor if their refactoring suggestions could break correctness.

### Quality Auditor
You own maintainability. Your job is to find code smells, missing tests, poor naming, inadequate documentation, and duplicated code. You think about the next developer (human or AI) who touches this code. Challenge the Technical Reviewer if their fixes introduce code smells. Challenge the Architect if their abstractions make the code harder to understand.

### Architect
You own system design. Your job is to evaluate patterns, abstractions, separation of concerns, and system fit. You look for over-engineering (YAGNI), under-engineering, and missed opportunities to use existing libraries. Challenge the Quality Auditor if they're polishing code that should be restructured. Challenge the Devil's Advocate if their "simpler approach" sacrifices important architectural properties.

### Devil's Advocate
You own assumptions. Your job is to question whether we're solving the right problem, whether there's a simpler approach, and whether code should be rewritten rather than fixed. You are explicitly adversarial. Challenge everyone. Ask "why?" and "what if we didn't?" and "is this even necessary?" If the code has accumulated significant debt, advocate for rewrite over fix.

### Agent UX Evaluator (when reviewing tools/CLIs)
You own the AI experience. Your job is to evaluate how well tools, APIs, and interfaces work for AI agents. Test familiarity, guessability, and cognitive boundaries. Simulate actual usage and report friction points. Challenge the Architect if their design creates unnecessary cognitive boundaries for agents.

## Review Protocol

1. Each teammate conducts their review independently first
2. Share initial findings with the team via messages
3. Challenge each other's findings — look for disagreements
4. Debate unresolved disagreements until consensus or clear tradeoff
5. The lead synthesizes all findings into a convergence report
6. Convergence is declared only when no teammate has unresolved concerns

## Convergence Criteria

The team has converged when:
- All teammates have completed their review
- Cross-team challenges have been resolved or documented as explicit tradeoffs
- No teammate has a blocking concern
- The lead can summarize: "This is as good as we can make it" with team agreement

## Output Format

Each teammate should structure findings as:
```
## [Role Name] Findings

### Critical (must fix)
- ...

### Important (should fix)
- ...

### Minor (nice to fix)
- ...

### Challenges to Other Teammates
- @[Role]: [challenge description]
```

The lead's final report should include:
- Summary of all findings by severity
- Resolved debates and their outcomes
- Unresolved tradeoffs with clear documentation
- Convergence declaration or reasons for additional iteration


<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:6cd5cc61 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.

> **Banshee-specific: `bd dolt push` DOES NOT WORK here, and it exits 0 while
> failing.** Beads sync is **JSONL-only**, and the export lives in the NOTEBOOK, not
> this tree: run **`bd export --include-memories -o private/beads/issues.jsonl`** and
> commit + push `private/` as part of session close (`AGENTS.md` → Landing the Plane).
> `.beads/` here is entirely gitignored (the local Dolt DB, config, hooks), so that
> exported file is the only durable copy of the issue graph.
>
> Why it fails (measured 2026-08-31): the remote is GitHub, and GitHub declines
> `refs/dolt/data` with *"push declined due to email privacy restrictions"*
> because Dolt's global identity is not the GitHub noreply address this repository
> commits under. **The block is correct and should not be worked around by
> relaxing GitHub's setting.** Setting a repo-local Dolt identity (done:
> `.beads/embeddeddolt/banshee/.dolt/config.json` carries the noreply address)
> fixes only *future* Dolt commits; the existing history still carries the old
> identity, and rewriting Dolt history is not worth it. Do not change the *global*
> Dolt identity either — other repositories legitimately want theirs.
>
> **`bd dolt push` returns exit 0 on this failure.** The error is stderr only, so
> a script or agent checking `$?` will conclude the push succeeded. Never treat a
> clean exit as proof of beads sync; read the output, or check the JSONL.
> A second Git host has shown the same exit-0-while-declining behaviour — assume
> it is the rule, not the exception.
>
> Both export flags matter. Without `--include-memories` the export is
> issues-only and every `bd remember` stays in the local DB. Without `-o` it
> prints to stdout, leaves the file untouched, and `git status` then says
> "nothing to commit" — which reads exactly like success. A stale export shows
> clean in `git -C private status`: the upstream template's own tracked export was
> found stale, and a sibling project's went three months stale.

## Agent Context Profiles

The managed Beads block is task-tracking guidance, not permission to override repository, user, or orchestrator instructions.

- **Conservative (default)**: Use `bd` for task tracking. Do not run git commits, git pushes, or Dolt remote sync unless explicitly asked. At handoff, report changed files, validation, and suggested next commands.
- **Minimal**: Keep tool instruction files as pointers to `bd prime`; use the same conservative git policy unless active instructions say otherwise.
- **Team-maintainer**: Only when the repository explicitly opts in, agents may close beads, run quality gates, commit, and push as part of session close. A current "do not commit" or "do not push" instruction still wins.

## Session Completion

This protocol applies when ending a Beads implementation workflow. It is subordinate to explicit user, repository, and orchestrator instructions.

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Handle git/sync by active profile**:
   ```bash
   # Conservative/minimal/default: report status and proposed commands; wait for approval.
   git status

   # Team-maintainer opt-in only, unless current instructions forbid it:
   git pull --rebase
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->
