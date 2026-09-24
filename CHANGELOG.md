# Changelog

All notable changes to Banshee. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/) as described in `VERSIONING.md`.

## [Unreleased]

### Added

- **A burst-drop disk alert** (banshee-636). Free space falling by 10 GB — or 2%
  of the volume TOTAL, whichever is smaller for the volume — within a rolling hour
  now forces disk at least yellow whatever the absolute level, with the drop named
  in the detail ("down 12.0 GB in the last 1h0m"). Calibrated against the
  2026-09-22 slide (43.8 → 9.2 GB at ~7 GB/hour), where it fires with ~30 GB still
  in hand. The fraction reads the volume total, never current free, so the rule
  does not go hypersensitive once free space is already small — there the byte
  bands own the alert. Direction-only: no forecast, so none of the projection's
  confidence problem.

- **A second, week-scale disk projection** (banshee-nio). Time-to-full read a
  ten-minute window, so it could only ever fire during a burst: 90 GB lost over
  7 days (~150 KB/s) projected "full in a week" and produced nothing — never
  yellow, never an episode, no banner. The disk dimension now also fits a least-squares
  trend over 7 days of retained rollups (`volumes[].availAvg`) and forces at least
  yellow when that rate forecasts full within 14 days, with the story in the detail
  line ("down 89.9 GB over 7d0h, full in ~7d18h at this rate"). Three horizons,
  deliberately separate (banshee-wql): the burst drop, the ten-minute projection,
  and the week trend — the week trend caps at yellow, never touches severity, and
  is trusted only once the rollups span at least half its window.

### Fixed

- **A sustained disk yellow now opens an episode** (banshee-dok). The daemon opened
  episodes only on red, so the 30–60 GB yellow band — exactly "leaving the target
  floor while action is still cheap" — never reached the episode log: the app's
  banner (0.1.7) spoke on yellow while `banshee alerts` said nothing. Disk yellow now
  opens behind its own long up-delay (30 minutes held, `BANSHEE_DISK_YELLOW_EPISODE_UP_SECS`),
  and a disk episode rides through yellow — one excursion is one incident, closed
  only on green, matching the app banner's single identifier. CPU and memory keep
  the red-only policy: they flap through yellow on every build, disk moves one way.
  To make the long hold measurable at all, the disk band now replays over a longer
  sample history (an hour by default); the ten-minute slope and projection windows
  are unchanged.

## [0.1.7] — 2026-09-23

One fix, cut on its own because it is the reason the maintainer's disk emergency
arrived unannounced.

### Fixed

- **A filling disk is named even when something else is redder** (banshee-3ue). The
  banner used to describe only the verdict's single dominant source, so on a machine
  where CPU, swap or thermal are red for hours a red disk produced a banner titled
  "Wailing, CPU" whose body described swap — which is how the maintainer reached under
  1 GB free with no warning he could see. The disk now has its own banner: on entering
  yellow (below 60 GB) and then at most every six hours, and on entering red (below
  30 GB) and then every fifteen minutes for as long as it stays red. The red disk
  banner and any Shrieking banner are time-sensitive, so they are presented at once
  and through Focus; before, no banner ever set an interruption level and every one
  auto-dismissed. Thresholds are unchanged. No critical-alert entitlement is used.

## [0.1.6] — 2026-09-20

The second release cut by the pipeline, one working day after the first; everything
here came out of watching that first update land.

### Added

- **Update reminders you can see.** When a scheduled update check finds a newer
  Banshee and cannot show the alert in front of you, the menu bar glyph wears a small
  dot and the popover's footer names the version; either brings the update window
  forward. "Check for Updates…" now sits in the popover beside Settings as well as on
  the status item's right-click menu, which was the only place before.
- **The daemon is told when the app has moved on.** An app update (Sparkle) replaces
  the app and leaves the background monitor where it was, and nothing errors — the
  app just quietly shows less than the daemon could tell it. The popover and the
  window now say when the two are different releases, name both versions, and offer
  "Update the daemon" (the app's own installer, from the daemon it carries).

### Fixed

- The release pipeline's attribution commit (third-party notices, SBOM) is now gated
  for residue before it is tagged; the first end-to-end release had committed it
  unchecked.
- Release builds name SwiftPM's native build system: the newer default (Swift 6.4)
  embedded absolute build-directory paths in the app binary, which the packaging scan
  correctly refused.

## [0.1.5] — 2026-09-15

Everything on `main` since 0.1.4, and the first release cut end to end by the build
server pipeline (`docs/build-server.md`) rather than by hand.

### Added

- **Alert episodes and the Recovering state** (schema v10, ADR-0009). An alert is
  one continuous incident on one dimension: it opens after the up delay, absorbs
  re-fires inside the repeat interval (counted, shown as "+N more"), survives short
  dips, and closes with exactly one recovery notice. A dimension that has dropped
  below red inside its episode's down window is `recovering`, and a calm glyph
  wears 🩹 for it.
- **The thermal dimension** (schema v11). The kernel's own thermal pressure level is
  the eleventh dimension: moderate is yellow, heavy and above red. A throttled
  machine runs slower for the same load figure, so a load red without this beside
  it was a story with a missing chapter.
- **Memory second pass** (schema v13). The kernel's reclaimable percentage and the
  compressor's occupancy are advisory dimensions; a jetsam kill is a confirmed kill
  and rings the top bell at once. Plain availability is now informational, never a
  driver, because banding on it alone produced false reds.
- **CPU banded on measured utilization** (schema v12), not load per core. The tick
  counters are differentiated between samples; load per core rides beside the
  headline as run-queue depth, and a deep queue with idle cores is reported as
  scheduling contention rather than saturation.
- **The who-line.** Every finding names the top consumers behind it, from the
  newest census — "who: Chrome ×115 at 7.7 GB, claude ×8 at 2.1 GB" — on the
  popover, `banshee status`, the MCP `pressure` tool and the alert text. Episodes
  keep the who-line at their peak.
- **The headroom read** (`/headroom`, `banshee headroom`, MCP `headroom`, ADR-0010).
  The verdict reduced to a decision for agents: wait or go, how many more workers
  the machine can absorb, when to ask again. Advice, never a block. Served on all
  three surfaces and pinned cross-language.
- **Deltas** (`/deltas`, `banshee deltas`, MCP `deltas`). What changed over a
  lookback, or since an alert episode opened, ranked by change rather than size and
  honest about any hole in the observations. Episodes now carry a compact census
  summary from their peak so the question still has an answer after the live
  censuses are swept.
- **Continuous history rollups.** Every closed five-minute bucket is rolled up as
  it closes, not only when it ages out, so the History tab's short windows are no
  longer empty by construction.
- **`/stats` says what Banshee itself costs**: physical footprint and CPU as a
  lifetime average over uptime, shown in the app and `banshee stats`.
- **`make install`** ships the daemon and the app together, from one commit, so the
  two halves cannot drift apart on the machine that runs them.
- **A self-contained release pipeline**: App Store Connect API-key notarization,
  key-file Sparkle signing, a named signing keychain and an enforced restore drill,
  so a release can be cut on a build server without the maintainer's login session.

### Fixed

- The daemon, CLI and MCP server report the same version as the app; the release
  gate blocks if the two version files diverge.
- One sustained red catastrophically past its line rings the top bell on its own,
  rather than waiting for a second dimension.
- The app decodes unknown `source` and `unit` values leniently, so a newer daemon
  does not blank a field in an older app; `level` and `band` stay strict on purpose.

## [0.1.4] — 2026-09-07

- The daemon refuses every credential sent over TCP; the API key travels only over
  the owner-only Unix socket (ADR-0008 Phase 3). The menu-bar app authenticates over
  the socket.
- A locked-out or long-unreachable app stops wearing its last verdict glyph: 🔒 at
  once on a refused key, 🫧 after a minute of silence.
- The thrash dimension bands on the peak of a five-minute window (median-filtered,
  then max), so it decays as a storm ages out instead of climbing.
- Clients re-read the API key on a 401, so a key rotation cannot strand them.
- `check-service.sh` probes the socket, verifies it is 0600, and keys only over it.

## [0.1.3] — 2026-09-03

- First launch offers to install the daemon from the helpers bundled inside the app,
  closing the gap between "dragged to /Applications" and "something is sampling".
- App icon, and a styled DMG window with the app-to-Applications layout.
- Third-party license notices ride in the DMG; a real build refuses to proceed
  without the Sparkle public key.
- The API key is bound to loopback destinations.

## [0.1.2] — 2026-09-02

- Sparkle auto-update embedded: a signed appcast published to GitHub Releases,
  EdDSA-verified before install. `publish.sh` signs the appcast and uploads it.
- The app is stapled inside the DMG, so an offline first launch passes Gatekeeper.

## [0.1.1] — 2026-09-02

- Hardened runtime on real Developer ID signing, so the app is notarizable; the
  notarized and stapled DMG verified end to end.
- API key-file hardening from the pre-publication security review: the blank-key
  and permission gaps closed, untrusted keys rotated, daemon logs owner-only.

## [0.1.0] — 2026-08-31

The first working build: the Rust core with the pressure model (bands, hysteresis,
six levels, ranked findings), the cheap-tier sampler and census tier, the SQLite
time-series store with retention and rollups, the HTTP API, the `banshee` CLI and
the MCP server at byte parity, the daemon as a launchd LaunchAgent, and the menu
bar glyph with its popover and detail window. Reap actions (preview, then execute)
and the optional Slack escalation sink followed the next day.

---

Versions 0.1.1 through 0.1.4 were interim builds installed on the maintainer's own
machine while the release pipeline was being brought up; none was git-tagged.
`scripts/release.sh` regenerates this file with `git-cliff` at release time.
