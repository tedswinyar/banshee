# ADR-0004: The collector runs as a launchd LaunchAgent, not a supervised child

- **Status**: accepted
- **Date**: 2026-08-31
- **Modifies**: [ADR-0001](0001-constellation-architecture.md) (supervision boundary only)

## Context

ADR-0001 says the Swift app supervises the API binary as a child process, and
lists "Supervision boundary: SwiftUI app → API binary (parent/child process)"
as a key boundary. The project template ships no launchd plist at all,
deliberately: for an on-demand tool, the API only needs to exist while a client
is asking it something, and a supervised child is simpler than a system service
(no install step, no stale plist, no `launchctl` in the mental model).

Banshee breaks that assumption. Its entire premise is that pressure accumulates
*while nobody is watching* — the 2026-08-04 incident (load 76 on 8 cores, swap
35.6/36 GB) built up over days of unattended agent sessions. A monitor that only
samples while its window is open cannot compute a trend across the window being
closed, and a trend is the one thing this app exists to produce.

Nor is "keep the app open" a fix. The app could be quit, or crash, or not be
launched after a reboot, and every one of those produces a gap in the time series
precisely when the machine is least healthy.

## Decision

We will run `banshee-api` — which owns both the HTTP surface and the sampling
loop — as a **launchd LaunchAgent**, and make the Swift app a client of it
rather than its supervisor.

Everything else in ADR-0001 stands unchanged: one writer, HTTP for every client,
key-file auth, loopback only, SQLite owned by the API alone. Only the
supervision boundary moves.

The service contract:

- Label `com.tedswinyar.banshee-api`, plist installed to `~/Library/LaunchAgents/`.
- `RunAtLoad`, `KeepAlive { SuccessfulExit: false }`, `ThrottleInterval 5`.
- `ProcessType Standard`. **Not** `Background`, and no `LowPriorityIO` — macOS
  throttles background I/O hard enough that a `ps`-walking sampler would miss
  its own schedule (measured on a third-party LaunchAgent that used that
  combination).
- Logs to `~/Library/Logs/banshee-api.log`.
- The binary is **installed** to `~/Library/Application Support/banshee/bin/`,
  copied as `.incoming` and moved into place with `mv` (`rename(2)` is atomic;
  `cp` over a live binary is `ETXTBSY` or a corrupt half-write).
- The plist **pins** the port via `BANSHEE_PORT` and the daemon does not climb the
  port ladder. The ladder stays for dev and test runs.

The Swift app does not launch the daemon on a normal path. It connects, and
treats "daemon not running" as a first-class state with a repair action — the
same posture ADR-0001 demanded for "server didn't start", reached differently.

## Consequences

- The time series survives app quit, logout, and reboot. That is the whole point.
- **A build directory is not an installation.** This is the sharpest edge the
  decision introduces, and it has already cost someone eight days: another
  project's plist named `rust/target/release/<its>-api` directly, a benchmark run
  recreated `target/release` without that binary, and the running process served
  from its deleted inode for eight days until a reboot exposed it. `launchd` then
  reported exit 78 — which is `EX_CONFIG`, and means "cannot exec the program",
  *not* "your plist is malformed". Two days of cycles were lost to that
  misreading. Guarded by `scripts/check-service.sh`, which reads
  `ProgramArguments[0]` with `PlistBuddy` (not grep — launchd rewrites plists in
  binary form) and fails if it points into `target/`.
- **Rebuilding no longer updates the running service; reinstalling does.** Every
  contributor has to learn this once. `make daemon-install` is the answer.
- Losing the port ladder for the daemon means a busy 18769 is a hard startup
  failure rather than a silent move. That is correct for a service whose own MCP
  server and menu bar must find it, and the failure is loud in the log.
- **Cost: a real install step.** The template promised "drag the .app from a DMG";
  Banshee needs the app *and* a registered agent. Mitigated by having the app
  offer to install the agent, and by `check-service.sh` telling the truth about
  machine state. Deliberately not part of `make verify` — verify asserts repo
  state, and machine state is a different question.
- macOS notification banners cannot come from the daemon: `UNUserNotificationCenter`
  requires a bundled app with a bundle ID and user authorization. So the daemon
  *records* alert events and the app *posts* them, while the Slack sink lives in
  the daemon so critical alerts still escape with the app closed. See ADR-0005 for
  why that split does not fragment the severity decision.
- **Escape hatch:** if the LaunchAgent proves more trouble than the gap it closes,
  `SMAppService.agent(plistName:)` with the plist shipped inside the bundle is a
  smaller step than reverting to a supervised child — it keeps the daemon
  independent of the app's lifetime while making install a bundle concern.
  Reverting to a child process means giving up trends, so it is not a real option.

## Refinements (2026-08-31, at implementation)

Recorded here rather than in a new ADR: none of these changes the decision, and a
reader who follows the plist to this file needs them in one place.

- **`BANSHEE_PORT`, not `BANSHEE_API_PORT`.** This ADR and the design notes both
  wrote `BANSHEE_API_PORT`; the implemented, documented and tested variable has
  always been `BANSHEE_PORT` (`rust/banshee-api/src/config.rs`, the profile table
  in `docs/daemon.md`, every test-profile invocation). The prose was the slip, so the prose was
  corrected — renaming the variable would have churned the e2e harness and the
  config tests to no benefit. Note the asymmetry with the CLIENT variables, which
  are `BANSHEE_API_URL` / `BANSHEE_API_KEY`: the port is a SERVER-side setting and
  reads as one.

- **"No ladder for the daemon" is implemented as "no ladder in the prod profile."**
  The ADR said the ladder stays for "foreground, dev, and test runs", which would
  have needed a fourth knob to distinguish a foreground prod run from the daemon.
  Profile turned out to be the honest discriminator, and slightly stronger:
  prod's port is the one `banshee_core::DEFAULT_API_PORT` promises to every client,
  so moving off it silently is a bug wherever it happens. A foreground prod run
  that finds 18769 busy is almost always a second instance, and failing loudly is
  the correct answer there too. Dev keeps the ladder so `./scripts/start.sh` never
  fights whatever is bound; test uses port 0 and has nothing to climb.
  Pinned by `only_the_prod_profile_pins_its_port` in `banshee-api/src/main.rs`.

- **`launchctl print` reports `EX_CONFIG`, not `78`.** Verified against a live
  agent on macOS 15: the numeric form this ADR quotes is what older releases print.
  `check-service.sh` matches both, because the whole value of that hint is that it
  fires — a branch keyed only on `78` was dead code on this machine.

- **The consequence about the Swift app was implemented as a DELETION.** ADR-0001's
  `APIServerManager` supervised `banshee-api` as a child; it is gone, along with the
  `ServerStartup` actor and its ten tests. Keeping a spawn path next to an
  installed agent is not a harmless fallback — it puts a SECOND writing process on
  the database, which is the one invariant ADR-0001 rests on, and the second
  instance would sample in parallel with nothing in the UI to show it. What
  replaced it (`DaemonService`) probes `/health`, keeps the announcement parser
  (still the only truthful source of a bound port), and answers "what should the
  user run" — the repair action this ADR asked for. The app's `.app` bundle still
  carries `banshee-api`, because a bundled copy is a legitimate SOURCE for an
  install; it is no longer something the app executes.

- **`uninstall-launchd.sh` was not in the plan and is not optional.** An install
  step with no uninstall is the stale-plist trap this ADR warns about, and "just
  delete the plist" leaves a running process serving from a deleted inode until the
  next reboot — the same shape as the eight-day outage, arrived at from the other
  direction. It boots out first, polls for the bootout to land, and never touches
  the database.
