# Threat model

Four standing questions, answered for what Banshee actually is today: a
loopback-only sampling daemon that owns a SQLite database AND can kill
processes (the reap actions) and dial out to one Slack webhook (the
escalation sink). Re-answer them when the domain grows a new surface — each
new surface invalidates parts of this analysis.

**This file provides the analysis; [../SECURITY.md](../SECURITY.md) states the
public-facing claims.** When you update this threat model, update SECURITY.md
in the same commit.

## 1. What are we protecting?

- **The sampling database** (`~/Library/Application Support/banshee/banshee.db`):
  integrity (no silent corruption), availability (no data loss). Its
  confidentiality matters less than a notes app's would — it is system metrics,
  process names and cwds, not personal documents — but a cwd or a process name
  can still be mildly sensitive.
- **The API key** (`…/banshee/api_key`, 0600): the local trust boundary. Whoever
  holds it can read every metric AND invoke the reap-execute routes, which kill
  processes. Protecting it is protecting the kill surface.
- **The Slack webhook URL**, when configured: a credential that grants anyone
  who holds it the ability to post into the user's Slack channel. It lives only
  in the LaunchAgent plist (0600) and the daemon's environment — never in the
  repo.
- **The machine itself**, indirectly: the reap-execute routes terminate real
  processes, so an attacker who reaches them can disrupt the user's work.

## 2. From whom?

| Adversary | In scope? | Why / mechanism |
|---|---|---|
| Other local users on the Mac | **yes** | key file, plist and socket are 0600 and the DB dir is the user's, so another account cannot read the key at rest. The daemon also binds a fixed *unprivileged* loopback TCP port that a hostile account could bind first and impersonate — so the file-loaded key travels ONLY over the owner-only Unix socket (ADR-0008), never over TCP, and the daemon REFUSES every credential on the port — valid or not — so nothing can come to depend on it (Phase 3). A client that finds no socket (a daemon older than itself) falls back to TCP WITHOUT the key and says so, rather than authenticating to whoever holds the port |
| Web pages hitting `127.0.0.1` (port scan / DNS rebinding / CSRF) | **yes** | all non-`/health` routes require `X-Api-Key`, a custom header — which forces a CORS preflight a cross-origin page cannot satisfy, so a browser without the key cannot even POST to the kill routes |
| A prompt-injected / hostile MCP agent | **yes** | reap-**execute** MCP tools are ABSENT from `tools/list` unless `BANSHEE_MCP_ENABLE_ACTIONS=1`, and the gate is re-checked on the call — an agent cannot invoke a kill it was never offered |
| Network attackers | no | server binds 127.0.0.1 only; no external listening surface |
| Processes running as the same user | no | same-user code can read the 0600 key (debugger, `/proc`-equivalent) and `kill` anything itself; that battle is the OS's, not ours |
| The developer shipping a bad migration / dirty daemon | **yes** | forward-only migrations + refuse-newer-schema + verified backups (`banshee backup`); `/health` serves the built git rev and the installer refuses a dirty tree, so "what is running" is answerable |
| Theft of the powered-off machine | no | FileVault's job; stated in SECURITY.md |

- **Anyone with write access to the repository, or ssh as the build user.** CI runs
  on a self-hosted runner (`docs/build-server.md`): every push to `staging`, `ci/**`,
  `feature/**` or `agent/**` executes repository code as the build user on the machine
  that holds the release signing material — the Sparkle private key, the notary key,
  the keychain password file. They are readable by that user, so a job can read them;
  the "locked" keychain is not a boundary against the job itself. There is no
  pull-request trigger, and `push`/`workflow_dispatch` require write access, so the
  boundary IS the collaborator list. Mitigation today: one maintainer, no outside
  collaborators, and the residue gate plus secret scanning on what gets pushed.
  Longer term: an unprivileged runner user for verify, with the release keys reachable
  only from `release/**` jobs.

## 3. What are the failure costs?

- **Killing the wrong process** is the sharpest cost the reap actions added. It is bounded
  by design: reap targets are recomputed at execute time (never taken from a
  caller), SIGKILL only reaches a pid that survived SIGTERM AND still matches as
  the same orphaned program, and the spare rails (busy pane, dirty git worktree,
  unknown idle age) fail safe. See `rust/banshee-core/src/actions.rs`.
- **Data loss > data disclosure** for the database, as before: backup and
  migration machinery is tested code; at-rest encryption is delegated to
  FileVault.
- **Credential disclosure** (API key, Slack URL): both at 0600, so the cost is
  realized only against same-user code, which is already out of scope.

## 4. What are we NOT protecting, and why it is fine

- **Process argv over the wire.** `OrphanCandidate.args` (a reap preview/execute
  response) carries the full command line so a person can tell which helper an
  orphan was. Command lines can in principle carry secrets (`--token=…`), but:
  (a) on macOS any local user can already read argv via `ps`, so this is not a
  new disclosure to the local-user adversary; (b) it reaches only a key holder
  (the user, or an explicitly actions-enabled agent); (c) it is NOT persisted —
  neither the census DB (which stores program basename + cwd, not args) nor the
  logs record it (`actions.rs` does no logging; the routes log only error
  values). **Forward-looking rule:** if a reap report is ever written to a log
  or the database, redact `args` first — the 0644 log would turn a transient
  `ps`-visible string into a persisted one.
- **`census.local.toml` confidentiality on a multi-user Mac.** The overlay is
  operator-authored (banshee only reads it) and holds site-specific process
  names. At its typical 0644 other local users can READ it (names leak) but not
  WRITE it (0755 dir; only the owner can replace it), so it cannot be used to
  widen the reap set by anyone but the same user. On a single-user machine this
  is moot; on a shared Mac, `chmod 600 census.local.toml` — banshee does not do
  it because it does not own the file's lifecycle.

## 5. What changes the model?

- **Already changed it (recorded here rather than re-litigated):** the reap
  routes added a kill surface (mitigations in §2/§3); the Slack escalation sink
  added one outbound call to a configured webhook (credential kept to the 0600
  plist); and Sparkle adds a scheduled auto-update check — an outbound HTTPS fetch
  of a signed appcast and package from this repository's GitHub Releases, verified against
  the app's embedded EdDSA public key before install. Residual risks: release-key
  compromise, version disclosure to the release host, and no independent downgrade
  prevention (see SECURITY.md).
- **Any NEW outbound network call** (a second sink, telemetry): update THIS
  FILE and SECURITY.md in the same commit; revisit the "no network calls" list.
- **Multi-device sync**: transport auth, at-rest posture, and conflict handling
  all enter scope — a new model, not an edit.
- **Attachments/exports**: exported files (including `banshee backup` snapshots
  copied off-machine) leave the trust boundary; document where they land.

_Keep the table honest. An adversary marked in-scope without a mechanism
enforcing it is marketing, not modeling._
