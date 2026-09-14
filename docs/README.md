# Documentation Map

This directory contains all project documentation. The right doc is always <30s away — use this map.

## First time here

Start here to get the app running, then fan out as needed:

1. **[getting-started.md](getting-started.md)** — clone to running app in 10 minutes
2. **[daemon.md](daemon.md)** — install it for real; why rebuilding does not update the running service
3. **[signal-collection.md](signal-collection.md)** — what Banshee measures, and how
4. **[adding-a-field.md](adding-a-field.md)** — the full touch-point checklist for changing the domain
5. **[glossary.md](glossary.md)** — project terminology (constellation, sentinel, alert episode, etc.)

## For contributors

If you're changing or extending the project:

- **[../CONTRIBUTING.md](../CONTRIBUTING.md)** — contribution model and bar
- **[../CODE_OF_CONDUCT.md](../CODE_OF_CONDUCT.md)** — community standards
- **[../VERSIONING.md](../VERSIONING.md)** — when to bump major/minor/patch
- **[adding-a-field.md](adding-a-field.md)** — exhaustive checklist for domain changes
- **[data-safety.md](data-safety.md)** — forward-only migrations, backup verification, test profile guards
- **[lessons/](lessons/)** — evidence-backed write-ups of what the project learned the hard way, and the bar a lesson must meet

## For release and operations

Shipping a version or investigating failures:

- **[daemon.md](daemon.md)** — install/update/diagnose the LaunchAgent; the plist and why each key is what it is
- **[build-pipeline.md](build-pipeline.md)** — DMG signing, notarization, release process
- **[release-checklist.md](release-checklist.md)** — the gates `release.sh` runs and the judgment calls it cannot
- **[build-server.md](build-server.md)** — the build-server runbook: relay, self-hosted runner, headless signing, how a release is cut and published
- **[troubleshooting.md](troubleshooting.md)** — common failures, diagnosis, fixes
- **[../SECURITY.md](../SECURITY.md)** — trust model, privacy stance, vulnerability reporting
- **[threat-model.md](threat-model.md)** — adversary analysis, what we protect and from whom

## For understanding the domain

What Banshee actually does, as opposed to how it is built:

- **[signal-collection.md](signal-collection.md)** — every signal, the command that produces it, the thresholds, and the parse traps. The spec the samplers implement; the faithful port of `perf-scan`.
- **[wire-format.md](wire-format.md)** — the exact bytes crossing the Rust/Swift boundary
- **[adr/0003-sentinel-not-explorer.md](adr/0003-sentinel-not-explorer.md)** — the sentinel/disk-tool boundary and the no-filesystem-walk rule

## For understanding the architecture

Deep dives:

- **[adr/](adr/)** — Architecture Decision Records (why the constellation, etc.)
- **[glossary.md](glossary.md)** — coined terms and project vocabulary
- **[ROADMAP.md](ROADMAP.md)** — directional view (where we're going, what's out of scope)
- **[lessons/](lessons/)** — evidence-backed write-ups of what the project learned the hard way, and the bar a lesson must meet
