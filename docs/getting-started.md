# Getting started — clone to running app in ~10 minutes

This is the human tour, from a fresh clone to a running app.

## Prerequisites

- macOS 14+, Xcode command-line tools (`swift --version` works)
- Rust (`cargo --version`)
- `jq`

## 1. Build and test everything

```bash
./scripts/verify.sh
```

One command, every suite: Rust unit + integration (incl. `cargo fmt` and
`clippy`), Swift unit, the e2e parity harness, and the scripts suite. Expect the
first run to take a few minutes (fresh cargo build); later runs are incremental.

## 2. Install the daemon

```bash
make daemon-install     # builds, installs, registers, and waits for /health
make daemon-status      # ✓ or a diagnosis with the fix
```

This is what makes Banshee a sentinel: the sampling loop has to be running while
nobody is watching, because every interesting signal is a rate. **Rebuilding does
not update it — `make daemon-install` does**, and it is idempotent. Full detail,
including why the plist must never point into `rust/target/`, is in
[daemon.md](daemon.md).

Not ready to register an agent? Skip this and use `./scripts/start.sh` (step 4),
which runs a throwaway dev-profile server in the foreground.

## 3. Run the app

```bash
make app                 # assemble build/Banshee.app (ad-hoc signed)
open build/Banshee.app
```

The app is a CLIENT of the daemon — it never starts one, because a second
`banshee-api` would be a second writer on the same database. With the agent
installed the window shows the verdict; without it, the window says so and names
the command to fix it.

Start at login is two halves: the daemon is a LaunchAgent with `RunAtLoad`, so it
always survives a reboot; the app registers itself as a login item (`SMAppService`)
once, on its first launch from `/Applications` — deliberately not from a `build/`
checkout, so a developer's throwaway build never starts at login. Settings › "Open at
login" reads launchd's answer and toggles it.

Once connected, the verdict is **Checking** for the first ~15 seconds, which is not
a claim that the machine is calm.

Impatient? `dev-build.sh` turns the sampling cadence up so a real verdict appears
in about a second:

```bash
./scripts/dev-build.sh   # debug build + warm the dev database + launch (dev profile)
```

No daemon at all, or you want the app to show something other than this machine?
`scripts/fixture-server.py` serves the shared wire fixtures in `tests/fixtures/` over a
Unix socket, so the real app renders synthetic data (it is how the README's screenshots
were taken):

```bash
mkdir -p /tmp/fx && printf fixture > /tmp/fx/key
python3 scripts/fixture-server.py --socket /tmp/fx/api.sock --pressure shrieking &
BANSHEE_API_SOCKET=/tmp/fx/api.sock BANSHEE_KEY_FILE=/tmp/fx/key build/Banshee.app/Contents/MacOS/Banshee
```

## 4. Poke it from the command line

```bash
./scripts/start.sh &                    # dev-profile API: its own socket, port 18779
export BANSHEE_API_SOCKET="$HOME/Library/Application Support/banshee/api-dev.sock"
cd rust
./target/debug/banshee status           # the verdict, rendered
./target/debug/banshee status --all     # …including the dimensions that are fine
./target/debug/banshee headroom         # should an agent start new work? wait/go, N workers
./target/debug/banshee census           # what is actually running
./target/debug/banshee samples --limit 5
./target/debug/banshee stats            # what Banshee itself costs
```

**Note**: `start.sh` runs the **dev** profile, which serves on its own Unix socket
(`api-dev.sock`, beside the prod daemon's `api.sock`) and on port 18779. Point the
CLI at the SOCKET with `BANSHEE_API_SOCKET`; without that it finds the prod
daemon's socket and talks to the real thing. Do not reach for `BANSHEE_API_URL`:
the key travels only over the socket, and the daemon refuses every key on the TCP
port (ADR-0008) — over TCP only `/health` answers. The port and socket literals
live in one place, `banshee_core`, so the server and every client cannot disagree
about them.

The CLI, the MCP server (`banshee-mcp`, wired in `.mcp.json`), and the app are
three clients of the same API, and they are held to BYTE-IDENTICAL answers:
`banshee census --json`, `curl $BASE/census` and the MCP `census` tool return the
same bytes, which `tests/e2e/run-e2e.sh` checks on every run. The data endpoints
are reads — the sampler is the only writer; a few authenticated POST routes run
local actions (reap, backup), never data writes.

## 5. Install the push gate

```bash
make bootstrap   # installs the pre-push hook: check-hooks + verify.sh
```

Every branch push now runs the full verify gate; `git push` on a red suite is
blocked by design. **You rarely need to run this explicitly** — `make build`,
`make verify` and `make release-build` self-heal the gate, reinstalling it
whenever it is missing or stale (git cannot run install code on clone, so this
is how a fresh clone or a second machine gets the hook without remembering a
step). `make ci` is the canonical "run the whole gate now" entry point.

**The gate is local, and that is deliberate** (single-dev tool; no paid remote
CI). Two bypasses exist and are *deliberate acts*, not
accidents: `git push --no-verify` skips the hook, and a GitHub web edit or a
push from a checkout that never ran a `make` target is never gated. If you use
one, you are choosing to; run `make ci` yourself before or after.

## 6. Where things are

| Want to… | Look at |
|---|---|
| Understand the architecture | `docs/adr/0001-constellation-architecture.md` |
| Understand the level scale and the glyphs | `README.md`, `docs/adr/0005-pressure-model-in-core.md` |
| Add a field end to end | `docs/adding-a-field.md` (the touch-point list) |
| Know what is measured, and how | `docs/signal-collection.md` |
| Change the wire format | `docs/wire-format.md` + `tests/fixtures/` first |
| Run it for real, or debug the daemon | `docs/daemon.md` |
| Ship a release | `docs/build-pipeline.md`, `VERSIONING.md` |

## Troubleshooting

- **verify exits 75** — another verify is running; wait, retry.
- **Push blocked with "cannot execute binary file"** — something rewrote the hook
  chain; run `./scripts/install-hooks.sh` (see troubleshooting.md).
- **App shows "Nothing is watching"** — the daemon is not answering. Run
  `make daemon-status`; the app deliberately does not start one.
- **The daemon is registered but not running** — read
  `~/Library/Logs/banshee-api.err.log`; the server's stderr lands there, not in
  the UI. `EX_CONFIG` / exit 78 means launchd could not exec the binary, not that
  the plist is bad.

For complete troubleshooting (build failures, runtime errors, test failures, migrations, releases), see **[troubleshooting.md](troubleshooting.md)**.
