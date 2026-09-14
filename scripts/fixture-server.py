#!/usr/bin/env python3
"""A stand-in daemon that serves the shared wire fixtures over a Unix socket.

For looking at the app (or driving the CLI/MCP) WITHOUT a real daemon and without this
machine's data: every name and number comes from `tests/fixtures/`, which are synthetic by
construction. Timestamps are shifted so the newest instant in each fixture is "now", so
the app does not show a stale verdict; the History tab gets six hours of generated rollup
buckets and the Alerts tab a handful of episodes derived from the episode fixture.

    scripts/fixture-server.py --socket /tmp/fx/api.sock [--pressure shrieking|quiet|jetsam|checking]
    printf fixture > /tmp/fx/key
    BANSHEE_API_SOCKET=/tmp/fx/api.sock BANSHEE_KEY_FILE=/tmp/fx/key build/Banshee.app/Contents/MacOS/Banshee

Speaks exactly what the Swift client expects (ADR-0008): HTTP/1.1, `content-length`, no
chunking, `connection: close`. Any `x-api-key` is accepted — there is nothing to protect.
Query strings are ignored. Unknown routes 404 with the daemon's `{"error": …}` shape.
"""

import argparse
import datetime as dt
import json
import os
import re
import socketserver
import sys
import uuid
from http.server import BaseHTTPRequestHandler

ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
FIXTURES = os.path.join(ROOT, "tests", "fixtures")
STAMP = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?Z")


def parse(s):
    frac = ""
    if "." in s:
        s, frac = s[:-1].split(".")
        s += "Z"
    base = dt.datetime.strptime(s, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=dt.timezone.utc)
    return base + dt.timedelta(microseconds=int((frac + "000000")[:6]) if frac else 0)


def fmt(t):
    return t.strftime("%Y-%m-%dT%H:%M:%S.") + "%06dZ" % t.microsecond


def shift_to_now(text, newest_age_secs=8):
    """Every timestamp in `text` moves by one constant so the newest is `newest_age_secs` ago."""
    stamps = STAMP.findall(text)
    if not stamps:
        return text
    newest = max(parse(s) for s in stamps)
    delta = (dt.datetime.now(dt.timezone.utc) - dt.timedelta(seconds=newest_age_secs)) - newest
    return STAMP.sub(lambda m: fmt(parse(m.group(0)) + delta), text)


def fixture(name):
    with open(os.path.join(FIXTURES, name + ".json"), encoding="utf-8") as fh:
        return fh.read()


def rollups():
    """Six hours of 5-minute buckets ending now, shaped like a real bad afternoon."""
    template = json.loads(fixture("rollups"))[0]
    now = dt.datetime.now(dt.timezone.utc).replace(second=0, microsecond=0)
    out = []
    n = 72
    for i in range(n):
        start = now - dt.timedelta(minutes=5 * (n - i))
        x = i / n
        spike = 11.0 * max(0.0, 1 - abs(x - 0.62) * 9) ** 2      # a sharp load spike ~2h ago
        load = 2.4 + 0.8 * (x > 0.3) + spike + 0.3 * ((i * 7) % 5) / 5
        swap = 6.0e9 + 11.0e9 * min(1.0, max(0.0, (x - 0.35) / 0.4))  # swap climbs and stays
        b = dict(template)
        b["bucketStart"] = fmt(start)
        b["load1mAvg"] = round(load, 3)
        b["load1mMax"] = round(load * 1.25 + 0.6, 2)
        b["swapUsedAvg"] = int(swap)
        b["swapUsedMax"] = int(swap * 1.04)
        b["orphansMax"] = 12 + int(40 * max(0.0, x - 0.5))
        b["staleSessionsMax"] = 9 + int(8 * x)
        out.append(b)
    return json.dumps(out)


def alerts():
    """A few episodes on different dimensions, from the episode fixture's shape."""
    base = json.loads(fixture("alert-episode"))
    now = dt.datetime.now(dt.timezone.utc)

    def ep(dimension, hours_ago, dur_min, state, level, band, message, who, suppressed, census=False):
        e = json.loads(json.dumps(base))
        start = now - dt.timedelta(hours=hours_ago)
        peak_at = start + dt.timedelta(minutes=dur_min * 0.6)
        end = start + dt.timedelta(minutes=dur_min)
        e.update({
            "id": str(uuid.uuid5(uuid.NAMESPACE_URL, "banshee-fixture-" + dimension + str(hours_ago))),
            "dimension": dimension,
            "startedAt": fmt(start),
            "endedAt": fmt(end) if state != "firing" else None,
            "suppressed": suppressed,
            "state": state,
            "lastNotifiedAt": fmt(peak_at),
            "recoveringSince": fmt(end) if state == "recovering" else None,
        })
        e["peak"].update({"band": band, "level": level, "at": fmt(peak_at), "message": message, "whoLine": who})
        if not census:
            e["censusAtPeak"] = None
        return e

    return json.dumps([
        ep("cpu", 0.7, 42, "firing", "wailing", "red",
           "CPU is saturated: cores are 98% busy.",
           "who: claude ×8 at 71% of one core, Google Chrome ×34 at 22% of one core", 3),
        ep("thrash", 3.2, 256, "closed", "shrieking", "red",
           "Peaked at 4145 swap operations per second — the machine is thrashing.",
           "who: Chrome ×102 at 8.9 GB, claude ×10 at 2.1 GB", 39, census=True),
        ep("swap", 5.1, 110, "recovering", "restless", "red",
           "17.5 GB of swap in use.", "who: Chrome ×88 at 7.4 GB, claude ×9 at 1.9 GB", 6),
        ep("orphans", 27.0, 61, "closed", "restless", "red",
           "45 orphaned helper processes — their parent sessions are gone.",
           "who: mcp-server ×31, language-server ×9", 0),
        ep("agents", 52.0, 900, "closed", "stirring", "yellow",
           "17 agent sessions idle for more than two days.", "who: claude ×14, codex ×3", 0),
    ])


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    pressure_variant = "shrieking"

    def log_message(self, fmt_, *args):  # one line per request, to stderr
        sys.stderr.write("fixture-server: %s\n" % (fmt_ % args))

    def _send(self, code, body):
        data = body.encode("utf-8")
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.send_header("connection", "close")
        self.end_headers()
        self.wfile.write(data)

    def _route(self, method, path):
        pv = self.pressure_variant
        table = {
            ("GET", "/health"): lambda: json.dumps({"status": "ok", "version": "0.1.4", "gitRev": "fixture"}),
            ("GET", "/pressure"): lambda: shift_to_now(fixture("pressure-" + pv)),
            ("GET", "/headroom"): lambda: shift_to_now(fixture("headroom-" + ("throttled" if pv == "jetsam" else pv))),
            ("GET", "/samples"): lambda: shift_to_now(fixture("samples")),
            ("GET", "/rollups"): rollups,
            ("GET", "/census"): lambda: shift_to_now(fixture("census-full")),
            ("GET", "/alerts"): alerts,
            ("GET", "/deltas"): lambda: shift_to_now(fixture("deltas-episode")),
            ("GET", "/stats"): lambda: fixture("stats"),
            ("POST", "/actions/reap-stale-sessions/preview"): lambda: fixture("reap-sessions"),
            ("POST", "/actions/reap-stale-sessions/execute"): lambda: fixture("reap-sessions"),
            ("POST", "/actions/reap-orphans/preview"): lambda: fixture("reap-orphans"),
            ("POST", "/actions/reap-orphans/execute"): lambda: fixture("reap-orphans"),
        }
        return table.get((method, path))

    def _handle(self, method):
        path = self.path.split("?", 1)[0]
        length = int(self.headers.get("content-length") or 0)
        if length:
            self.rfile.read(length)
        fn = self._route(method, path)
        if fn is None:
            return self._send(404, json.dumps({"error": "no such route: %s %s" % (method, path)}))
        self._send(200, fn())

    def do_GET(self):
        self._handle("GET")

    def do_POST(self):
        self._handle("POST")


class Server(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    allow_reuse_address = True
    daemon_threads = True


def main(argv):
    p = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    p.add_argument("--socket", required=True, help="Unix socket path to listen on (created; parent dir must exist)")
    p.add_argument("--pressure", default="shrieking", choices=["shrieking", "quiet", "jetsam", "checking"],
                   help="which pressure fixture to serve (default shrieking)")
    args = p.parse_args(argv)
    Handler.pressure_variant = args.pressure
    if os.path.exists(args.socket):
        os.unlink(args.socket)
    srv = Server(args.socket, Handler)
    os.chmod(args.socket, 0o600)
    print("fixture-server: serving %s fixtures on %s" % (args.pressure, args.socket), flush=True)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        srv.server_close()
        if os.path.exists(args.socket):
            os.unlink(args.socket)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
