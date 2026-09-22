# Running Banshee as a daemon

Banshee's premise is that pressure accumulates *while nobody is watching* — the
reference overload (2026-08-04: load 76 on 8 cores, swap 35.6/36 GB) built up over days of
unattended agent sessions. **Every interesting signal here is a rate, and a rate
needs a process that was already running.** So `banshee-api` — which owns both the
HTTP surface and the sampling loop — runs as a launchd LaunchAgent, and the app is
a client of it rather than its supervisor ([ADR-0004](adr/0004-collector-as-launchagent.md)).

Until it is installed, Banshee is a nicer `perf-scan`.

## The three commands

```bash
make install            # daemon + CLI AND the app in /Applications, relaunched — use THIS
make daemon-install     # the daemon half alone (install, or UPDATE after any change). Idempotent.
make daemon-status      # what is actually running, and what is wrong with it
make daemon-uninstall   # unregister. Never touches the database.
```

`make install` exists because the two halves only agree when built from one commit, and
the wire format is lenient enough that a lagging app never fails — it just shows less than
the CLI does. Ship both, every time (maintainer decision, 2026-09-13).

Also: `make daemon-restart` (restart without reinstalling) and `make daemon-logs`.

## The one thing everybody gets wrong once

**An app update does not update the daemon either.** Sparkle replaces the app bundle
and knows nothing about the LaunchAgent; the app compares `/health`'s version with its
own on every (re)connection and, when the daemon is behind, offers "Update the daemon"
in the popover — the same `DaemonInstaller` a DMG user's first install runs, from the
daemon in `Contents/Helpers`. On a source checkout, `make daemon-install`.

**Rebuilding does not update the running service. Reinstalling does.**

`cargo build` writes to `rust/target/`. The LaunchAgent runs an *installed* copy at
`~/Library/Application Support/banshee/bin/banshee-api` and deliberately never
looks at `target/`. The `banshee` CLI and the `perf-scan` shim install alongside
it, so a retired `perf-scan` symlink keeps working after a `cargo clean`. So after any change you want the daemon — or the shim's CLI — to
serve:

```bash
make daemon-install
```

It rebuilds, copies the binary as `.incoming`, moves it into place with `mv`
(`rename(2)` is atomic; `cp` over a live binary is `ETXTBSY` or a corrupt
half-write), rewrites the plist, boots the old service out, bootstraps the new one,
and then **polls `/health` until it answers** rather than assuming.

### Why not just point the plist at `target/release/banshee-api`?

Because a build directory is not an installation: a rebuild or a `cargo clean`
can remove the binary under a running service, which then serves from a deleted
inode until a reboot exposes it. `launchd` then reports exit **78**, which is
`EX_CONFIG` — "cannot exec the program", *not* "your plist is malformed". The
full story is in [ADR-0004](adr/0004-collector-as-launchagent.md).

`scripts/check-service.sh` refuses any plist whose `ProgramArguments[0]` contains
`target/`, and `scripts/tests/test-service.sh` pins that refusal.

## Reading `make daemon-status`

It answers these questions, in order, because they have different fixes:

| Check | What a failure means |
|---|---|
| plist present & valid | not installed → `make daemon-install` |
| program is not a build directory | the deleted-inode trap above (ADR-0004) |
| program exists and is executable | the exit-78 shape → reinstall |
| profile/port/ProcessType pinned | a hand-edited plist → reinstall |
| registered with launchd, and running | read `~/Library/Logs/banshee-api.err.log` |
| answering `/health` on TCP | the process is up but cannot serve |
| **the socket exists and is `0600`** | no socket → the daemon predates ADR-0008 and every client is locked out → `make daemon-install`; wrong mode → any local user could connect, and the daemon should have refused to serve it |
| answering `/health` on the socket | a socket file with nothing behind it is a leftover from a crash → `launchctl kickstart -k` |
| **has stored samples** (asked over the socket, with the key) | serving but not *sampling* — the silent failure this whole app exists to avoid |

Exit codes: `0` healthy, `1` problems (each printed with a fix), `2` not installed.

**It is deliberately not part of `make verify`.** Verify asserts *repo* state;
whether a LaunchAgent is registered on this laptop is a different question with a
different answer on every machine. What verify *does* check is the install and
check scripts themselves, against a temp directory
(`scripts/tests/test-service.sh`).

## What the plist says, and why

| Key | Value | Why |
|---|---|---|
| `Label` | `com.tedswinyar.banshee-api` | one place knows it: `scripts/lib/service.sh` |
| `ProgramArguments[0]` | the installed binary | never `target/` — see above |
| `BANSHEE_PROFILE` | `prod` | the real database and the real port |
| `BANSHEE_PORT` | `18769` | **pinned**; prod does not climb the port ladder. TCP serves `/health` ONLY — every client authenticates over the 0600 socket `api.sock` beside the key file, and the daemon refuses any key sent to the port (ADR-0008) |
| `RunAtLoad` | true | survives a logout and a reboot |
| `KeepAlive.SuccessfulExit` | false | restart a *crash*, respect a clean exit — so `bootout` actually stops it instead of racing a respawn |
| `ThrottleInterval` | 5 | a crash loop must not spin |
| `ProcessType` | `Standard` | **not** `Background`, and no `LowPriorityIO` |

Two of those deserve more than a row:

**`ProcessType Standard`, and no `LowPriorityIO`.** macOS throttles background I/O
hard enough that a `ps`-walking sampler misses its own schedule — measured on
a third-party LaunchAgent that used that combination, where an I/O-bound job was
still running after 20 minutes. A sampler that misses its 15-second tick is not
sampling, and the gaps look exactly like a calm machine.

**The port is pinned, and prod does not climb the ladder.** 18769 is what
`banshee_core::DEFAULT_API_PORT` promises to every client — the CLI, the MCP
server, the app. A daemon that quietly moved to 18770 would leave all three asking
18769 and getting nothing, while `launchctl` reported a healthy service. So a busy
port is a hard startup failure, loudly, in the log. Dev keeps the ladder (so
`./scripts/start.sh` never fights the daemon); test uses port 0.

## Slack escalation alerts (optional)

By default, alerts live in the app: the menu bar glyph, a banner at Wailing (and a
banner for the disk alone whenever it leaves green, whatever else is red), and
the year-long episode log. To ALSO push the most severe notices — an episode
opening, re-escalating or repeating while the machine is Shrieking, and the ONE
recovery notice for an episode that peaked at Shrieking (ADR-0009) — to a Slack
channel, give the daemon an incoming-webhook URL:

```bash
BANSHEE_SLACK_WEBHOOK_URL='https://hooks.slack.com/services/…' make daemon-install
```

That writes the URL into the LaunchAgent plist's `EnvironmentVariables` (under
`~/Library/LaunchAgents`, outside this repo) and nowhere else — **the webhook is a
credential and must never be committed.** Unset it and re-run `make
daemon-install` to turn the sink back off. The daemon logs which mode it is in at
startup (`Slack escalation sink enabled` / `no Slack webhook configured`).

Delivery is **best-effort and never blocks sampling**: a slow or dead webhook is
logged and dropped, because a monitor that stops watching because Slack is down is
the exact failure it exists to prevent. Only Shrieking escalates — Wailing already
has a banner, and duplicating it into Slack is how a channel gets muted. A still-
firing episode repeats at most once an hour (`BANSHEE_EPISODE_REPEAT_SECS`), and
re-fires swallowed inside that hour are counted on the episode and rendered as
"+N more" rather than sent. The same env var works for a foreground
`./scripts/start.sh` too.

The episode lifecycle's timings are overridable in seconds for dogfooding —
`BANSHEE_EPISODE_UP_SECS` (default 120: how long a dimension must hold red before
an episode opens), `BANSHEE_EPISODE_DOWN_SECS` (default 600: how long it must stay
below red before the episode closes; a red inside this window REOPENS the same
episode) and `BANSHEE_EPISODE_REPEAT_SECS` (default 3600). Like the cadence
overrides, a bad value is a warning and the default, never a crash.

## Running a second instance on purpose

The daemon holds the prod database. To poke at something without disturbing it:

```bash
./scripts/start.sh          # dev profile: its own DB (banshee-dev.db), port 18779
./scripts/warm-dev-db.sh    # …and enough history to produce a real verdict
```

Point clients at its socket:

```bash
export BANSHEE_API_SOCKET="$HOME/Library/Application Support/banshee/api-dev.sock"
```

Not at its port. TCP is `/health` only — the daemon refuses every key sent over
it (ADR-0008 Phase 3), so a client given `BANSHEE_API_URL=http://127.0.0.1:18779`
is correctly locked out. `warm-dev-db.sh` and `dev-build.sh` set the socket for
you.

**Do not run a second *prod* instance.** Two writing processes on one SQLite file
is the invariant ADR-0001 rests on, and the second one will fail to bind 18769 and
exit — which is the design working, not a bug to route around.

## Troubleshooting

- **`launchctl print` shows `last exit code = EX_CONFIG`** — that is 78, and it
  means launchd could not *exec* the program. The plist is fine; the binary it
  names is missing. `make daemon-install`.
- **Nothing on 18769, but `launchctl` says it is running** — read
  `~/Library/Logs/banshee-api.err.log`. A pinned port that is already bound exits
  with a message naming `check-service.sh`.
- **A client says it is locked out, or a `curl -H X-Api-Key` to the port gets 401
  with "credentials are not accepted over TCP"** — that client is talking to the
  port. Since ADR-0008 Phase 3 the daemon accepts the key over its Unix socket
  ONLY; a client older than that (an app bundle from before 0.1.4, a script with
  a URL) keeps sending it to the port and is refused by design. Update the client;
  for a script, use `curl --unix-socket ~/Library/Application\ Support/banshee/api.sock`.
  The refusal is logged once per daemon process in the error log.
- **The app says "Nothing is watching"** — it probes `/health` and never starts a
  daemon (doing so would put a second writer on the database). The message carries
  the command to run.
- **`make daemon-status` says "answering but has stored NO samples"** — the process
  serves requests and knows nothing. Look for `failed to read the kernel` in the
  error log; a failed sample is logged and the loop continues by design, so this
  can persist quietly.
- **A plist read with `grep` finds nothing** — launchd rewrites plists in binary
  form once loaded. Use `/usr/libexec/PlistBuddy -c 'Print :Key'`.
