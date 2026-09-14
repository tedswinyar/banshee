# ADR-0008 — The API key travels over a Unix-domain socket, not a TCP port

**Status:** accepted (Phase 1 — daemon, CLI, MCP — landed 2026-09-06; Phase 2 — the Swift app — landed 2026-09-07; Phase 3 — the server refuses the key over TCP — landed 2026-09-07)
**Supersedes nothing. Amends:** ADR-0001 (constellation) and ADR-0004 (LaunchAgent),
both of which describe the pinned loopback port as the only transport.

## Context

Every client authenticates to `banshee-api` with a bearer key read from
`~/Library/Application Support/banshee/api_key` (0600). Until now that key was sent
over a **fixed loopback TCP port** (18769), gated only by a check that the destination
host was loopback.

**Loopback is not a sufficient gate, and the gate was the whole control.** Any local
user can bind `127.0.0.1:18769` before the daemon starts. Every client would then
connect to that process and hand it the key in an `X-Api-Key` header — a key that
authorises the reap/kill routes. The impersonator also gets to serve fabricated
verdicts to the menu bar. Nothing in the design detected it: the port is the daemon's
identity, and a port is first-come-first-served.

Related exposures were closed earlier (`no_proxy`, no redirect-following, refusing to
attach the key to a non-loopback URL, rotating a key whose file had become readable).
Those all narrowed *where* the key could go. None of them could fix *who is listening*.

## Decision

**The file-loaded key travels over a Unix-domain socket and nowhere else.**

1. The daemon binds `<config_dir>/banshee/api.sock` and serves the same `Router` there
   as on TCP. The socket is `0600`, in a `0700` directory.
2. **The transport decides whether the key may travel — not the hostname.** Over TCP,
   clients send only an *explicit* `BANSHEE_API_KEY`, which is the caller's own
   credential to spend. The file-loaded key is never sent over TCP, not even to
   loopback. Since Phase 3 the **server** enforces the same rule from its side and
   refuses every credential on the port — see below for why "explicit" cannot be
   honoured there.
3. Clients find the socket via **`BANSHEE_API_SOCKET`**, or the default path. An
   explicitly requested `--api-url` / `BANSHEE_API_URL` still wins, and gets no key.
4. TCP stays bound in every profile and serves **`/health` only**. Every other route
   answers 401 there with a body that names the transport, whether or not a key was
   presented (Phase 3). It carries no credential, so leaving it up does not reopen
   the hole; what it still provides is a liveness probe and a legible failure for
   stale clients.
5. **The key is kept.** On a 0600 socket the filesystem is the authenticator and the
   key adds little, but removing it would churn every client for no gain, and defence
   in depth is cheap here. It is no longer *the* boundary.

## Why a socket closes it

A socket file cannot be squatted the way a port can: it lives in a directory only this
user can write, so another user cannot create it, and cannot remove ours to plant
their own. Two facts were **verified by execution rather than assumed**, because the
whole claim rests on them:

- **macOS enforces the socket's mode on `connect`.** `chmod 000` on a listening socket
  denies `connect` even to its *owner*, with `EACCES`. Some BSDs ignore the mode
  entirely, in which case this design would have been decoration.
- **`bind()` creates the socket `0755`** under the usual umask — connectable by
  everybody. The mode must be set afterwards, which leaves a window; the `0700` parent
  directory is what closes it. The daemon does both, then **verifies** the result and
  refuses to serve if the socket is group/other-accessible, because failing to tighten
  it is precisely the vulnerability this transport exists to remove.

## Consequences

### The stale-socket problem is now ours

A crashed daemon leaves the socket file behind and `bind` fails with `EADDRINUSE`.
Deciding by "does the file exist" would let a second daemon unlink the socket a
*running* daemon is serving on — clients would keep talking to a deleted inode while
the new daemon looked healthy. That is the deleted-inode outage ADR-0004 describes,
reached by a new mechanism. So the daemon **tries to connect first**: success means somebody is serving
and this process is the mistake (exit 1, pointing at `check-service.sh`); connection
refused means the file is a leftover and is unlinked.

The socket is deliberately **not** removed on graceful shutdown. A crash cannot clean
up after itself, so the stale path has to be correct anyway — and making the tidy path
depend on graceful shutdown would leave the messy path the one that never runs.

### Two announcements, and a parsing hazard

The daemon prints both transports on stdout, on ADR-0004's principle that the
configured value is a request and only the announcement is the truth:

```
banshee-api listening on http://127.0.0.1:18769
banshee-api listening on unix:///Users/…/banshee/api.sock
```

**This broke the e2e harness**, which matched `listening on` and silently got a
two-line value. Every consumer must anchor on its scheme. Noted here because the next
person to add a transport will reintroduce it.

### `sun_path` is 104 bytes on macOS

A hard kernel limit; `bind` fails with `EINVAL` past it rather than truncating
visibly. The prod path is ~60 bytes and a socket under `TMPDIR` in a test is ~79, so
it fits with little headroom — a test nesting a few directories deeper can fail for a
reason that looks nothing like "path too long". `socket_path_fits` checks before both
`bind` and `connect` so the error names the real problem.

### `reqwest` cannot do this, so we own ~100 lines of HTTP

`reqwest` has no Unix-socket support, and the usual answer (`hyperlocal`) is async —
which would mean giving two synchronous binaries a tokio runtime for one connect call.
`banshee_core::uds_http` is a blocking HTTP/1.1 client instead. It is small **because
the server is ours**: measured against the real daemon, axum answers every route with
`content-length` and never `transfer-encoding: chunked`, so there is no chunked
decoder, and `Connection: close` makes read-to-EOF a correct fallback. It lives in
core for the same reason `DEFAULT_API_PORT` does — two hand-rolled copies would drift,
in the code that carries the key.

### The Swift app was Phase 2

`URLSession` **cannot** speak to a Unix socket at all — no scheme, no proxy trick, no
configuration key. Network.framework can (`NWEndpoint.unix(path:)` + `NWConnection`),
so the app carries its own small HTTP/1.1 client, `BansheeCore/UnixSocketHTTP.swift`,
behind `APIClient`'s existing `attempt()` seam: the whole `APIClient` surface, its
401 re-read-and-retry, and every existing test survived unchanged. It mirrors
`uds_http`'s decisions rather than just its shape (truncated body is an error, empty
key sends no header, the 104-byte limit is checked before connecting), and its
framing is proven against a real in-process Unix listener, not a mock.

`APIClient.resolve` makes the transport decision the same way the Rust clients do —
explicit `BANSHEE_API_URL` wins and carries no file key; else `BANSHEE_API_SOCKET` or
the default socket path, if it exists; else TCP without the file key — and is a pure
function of an injected environment and filesystem, so the one security decision in
the client is the easiest thing in it to test. Phase 1 therefore shipped without a
working menu bar (the CLI and the MCP server were the two clients that could leak the
key); Phase 2 restored it.

### Phase 3: the server refuses the key over TCP

Phases 1 and 2 fixed every client this repository ships, and measured immediately
afterwards the daemon still answered `curl -H X-Api-Key … http://127.0.0.1:18769/pressure`
with 200. That is the hole seen from the other side: any client that predates the
socket — any client older than 0.1.4 — kept working *because* it was sending the
file key to the squattable port and the daemon said yes. A hole that every shipped client avoids
but the server still honours is closed only in the clients that happen to be current.

**Decision: TCP serves `/health` and nothing else, in every profile.** The two options
were "drop the prod TCP listener" and "keep it for `/health`"; `/health` won, for a
stale client's sake. A client that predates the socket sends its key to the port. If
the port is closed it sees *connection refused*, which every client renders as "the
daemon is not running" — false, and it sends the user to inspect a healthy
LaunchAgent. If the port answers 401 the client lands in the path it already has for
"the daemon is running and this client is locked out", which is the truth, and the
body says why: `credentials are not accepted over TCP; every authenticated route is
served on the daemon's Unix socket`. The status is 401 rather than 403 or 404 for the
same reason — it is the one status every existing client already treats as
"locked out, daemon fine". `/health` itself returns nothing secret and accepts no
credential, so a squatter serving a fake one fools a liveness probe and nobody else.

**Explicit keys are refused too.** Decision 2 lets a client spend an *explicit*
`BANSHEE_API_KEY` over a URL it was explicitly pointed at, and that rule stands on
the client. The server cannot honour it: there is one key, so the daemon has no way
to tell "the caller typed this key" from "the caller's old client read it from the
file", and "allow explicit keys over TCP" is therefore "allow the key over TCP". A
`BANSHEE_API_URL` now reaches `/health` and nothing more, which is what it is for.

**How it is built, and what is pinned.** The router cannot learn from a request how it
arrived, so `build_router` takes a `Transport` and `main` builds one flavour per
listener: the socket's router checks the key, the port's router refuses before looking
at it. That pairing is the security decision, and swapping the two arguments would
put the key back on the port with every unit test still green, because the unit tests
build their own routers. It is pinned in three places: an integration test drives one
`AppState` through both flavours on a real port *and* a real Unix socket with the same
key (refused on one, served on the other, `/health` still open, a wrong key still
rejected on the socket); the e2e harness does the same against the shipped binary;
and the recorded mutation set has an entry for each, including one that hands the
TCP listener the socket flavour, which only the e2e harness can catch — the mutation
harness grew an `e2e` arm for it.

**Consequences.** The e2e harness's HTTP column moved to the socket
(`curl --unix-socket`), because a keyed request over TCP is no longer a read. The dev
profile is reached by socket too — `BANSHEE_API_SOCKET=…/api-dev.sock`, not
`BANSHEE_API_URL=…:18779` — and `warm-dev-db.sh` / `dev-build.sh` set it. The daemon
logs the first refused TCP request per process at `warn` and then stays quiet: a stale
menu-bar app polls every few seconds, and a warning per poll would bury the log. It
is a log line rather than a `/stats` field because the audience is the operator
looking for why an old client stopped working, and the client's own 401 already tells
the user.

### Upgrade behaviour

A new client against an old daemon finds no socket, falls back to TCP, sends no key
and reports a clear 401 naming the fix. An **old client against a new daemon** — the
Phase 3 case — sends the key to the port, is refused with a 401 naming the socket, and
shows its own locked-out state; the fix is to update the client. That is a deliberate trade: the alternative —
falling back to TCP *with* the key — would preserve the hole for exactly the case an
attacker can arrange (no daemon running, port free). In the app this is the "locked
out" state with a message that names `make daemon-install`, and it clears on its own
once the daemon is updated, because the app resolves its transport on every connect.

## Alternatives rejected

- **Keep TCP, verify the server's identity before authenticating.** There is no
  portable way to get the peer's uid for a loopback TCP connection on macOS, and
  anything based on inspecting the listening process is racy.
- **A random port written to a 0600 file.** An attacker who cannot read the file does
  not know the port, but nothing stops them binding many. Obscurity, not a boundary.
- **Encode the socket into `BANSHEE_API_URL`** as `http+unix://%2F…`. Percent-encoding
  a filesystem path is a reliable footgun, and that string is what `host_is_loopback`
  parses to decide whether the key may be attached — overloading it invites the exact
  confusion the gate exists to prevent. Hence a separate `BANSHEE_API_SOCKET`.
- **Drop the key once on a socket.** Correct in theory; churns every client and every
  test for no security gain, and leaves nothing if the socket's mode is ever wrong.
