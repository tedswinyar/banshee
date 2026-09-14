# Security

## Privacy stance

**No telemetry. No analytics. No network calls except those listed here.**

- The app, CLI, and MCP server talk only to the local `banshee-api`
  process, over a Unix socket in the user's own data directory (ADR-0008); the
  loopback TCP listener remains for the open `/health` probe.
- The API server never dials out, with ONE opt-in
  exception: when `BANSHEE_SLACK_WEBHOOK_URL` is configured, escalation alerts
  POST to that Slack webhook. It is off by default, and the URL is a credential
  stored only in the 0600 LaunchAgent plist and the daemon's environment —
  never in this repo.
- **Auto-update (Sparkle).** The APP (not the daemon) embeds Sparkle and, by
  default in a release build, checks for updates on Sparkle's schedule, fetching a
  signed appcast over HTTPS from this repository's GitHub Releases. Disable the
  scheduled check with `defaults write com.tedswinyar.banshee
  SUEnableAutomaticChecks -bool NO`; "Check for Updates…" still works by hand.
  Update packages are EdDSA-signed and Sparkle verifies the signature against the
  public key baked into the app (`SUPublicEDKey`) before installing, so an
  unsigned or tampered build is rejected. Residual risks worth knowing: a
  compromise of the release signing key would let an attacker ship a trusted
  update; the update check discloses the running version to the release host; and
  a downgrade to an older *signed* build is not independently prevented. Only the
  appcast fetch and package download are outbound — there is no telemetry.

## Trust model

The `X-Api-Key` key file (`0600`, generated locally) is a **local trust
boundary**: it keeps stray browser JavaScript and casual cross-account access
away from the API. It is NOT protection against network attackers — the server is
loopback-only by design — nor against anything running as your own user.

> **The key travels over a Unix-domain socket, never over TCP** (ADR-0008). The
> daemon also listens on a fixed, *unprivileged* loopback TCP port, and a hostile
> OTHER local account can bind that port while the daemon is stopped and
> impersonate it — so no client ever sends the file-loaded key there. The CLI and
> the MCP server authenticate over `~/Library/Application Support/banshee/api.sock`
> (`0600`, in a `0700` directory; the daemon verifies the mode before it serves).
> Over TCP a client sends only a key the caller supplied explicitly. The menu-bar
> app uses the same socket (Network.framework; `URLSession` cannot). A client that
> finds no socket — a daemon older than the client — falls back to TCP *without*
> the key and reports the mismatch, rather than authenticating to whoever holds the
> port.

Since P7 the API can also **kill processes** (the reap-execute routes). Those
routes are keyed like every other write; the destructive MCP tools are absent
from the tool list unless `BANSHEE_MCP_ENABLE_ACTIONS=1`; and targets are
recomputed at execution time, never accepted from a caller. So the key is not
just read access — it is the kill surface, which is why it (and the plist that
can carry the Slack credential) are held at 0600.

Data at rest is an unencrypted SQLite file under
`~/Library/Application Support/banshee/`; protecting it is FileVault's
job, not this app's. `banshee backup` produces a verified snapshot beside it;
a snapshot copied to external media leaves this boundary — treat it like the DB.

**See [docs/threat-model.md](docs/threat-model.md) for the complete adversary
analysis**, including what we protect, from whom, failure costs, and what
changes the model. When you add a new surface (network sync, sharing, cloud
backup), update BOTH files in the same commit.

## Gates that carry security

CI runs on the maintainer's self-hosted runner (`docs/build-server.md`): pushes by
collaborators only, no pull-request trigger. The gates below are what it enforces,
and they run locally too:

- `cargo deny check` (advisories + licenses; `deny.toml`) runs inside
  `verify.sh`'s rust suite when cargo-deny is installed — install it, or the
  advisory gate is decorative.
- `gitleaks` config ships at `.gitleaks.toml` for secret scanning
  (`gitleaks detect` before publishing anything).
- Release binaries are code-signed; DMGs are notarized when
  `scripts/release.conf` is configured (`docs/build-pipeline.md`).

## Reporting a vulnerability

Personal project. Please report security issues privately through GitHub's
private vulnerability reporting (the repo's **Security → Report a vulnerability**
tab) rather than opening a public issue with exploit details.
