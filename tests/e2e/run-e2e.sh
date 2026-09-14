#!/usr/bin/env bash
set -euo pipefail

# run-e2e.sh — the parity harness: two-implementations-one-database as a
# day-zero gate.
#
# THE PARITY TABLE below is the contract. Every read has exactly one HTTP route,
# one CLI subcommand, and one MCP tool, and all three must return the same JSON.
# The template's version checked one row through three surfaces; this checks all
# eight reads, because a sentinel whose surfaces disagree about whether the machine
# is on fire has destroyed the reason to trust any of them (ADR-0005).
#
# "The same" means the same BYTES — no normalization, not even key-sorting. That
# is achievable because the CLI and the MCP server relay the API's bytes rather
# than re-serializing a parsed value, and it is a much stronger claim than
# comparing decoded values: it catches casing, ordering, precision and null-shape
# drift in one assertion. Section 2 records the float bug that made the difference
# matter.
#
# Determinism, without a sleep: the daemon runs with an overridden cadence
# (BANSHEE_SAMPLE_INTERVAL_MS / BANSHEE_CENSUS_INTERVAL_MS), so it settles in
# under a second and takes exactly ONE census for the whole run — which makes
# /census a large, deeply nested, FROZEN payload and therefore the strongest
# byte-parity subject in the harness. A fixed sleep would encode an assumption
# about machine speed and be wrong in both directions.
#
# Requires debug binaries (cargo build --workspace); verify.sh runs the
# rust suite first, so they exist by the time this runs under verify.

ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="$ROOT_DIR/rust/target/debug"
WORK="$(mktemp -d /tmp/banshee-e2e.XXXXXX)"
API_PID=""

fail() {
  echo "e2e: FAIL: $*" >&2
  finish 1
}

cleanup() {
  if [ -n "${API_PID:-}" ]; then
    kill "$API_PID" 2>/dev/null || true
    wait "$API_PID" 2>/dev/null || true
  fi
  rm -rf "${WORK:-}"
}
# On macOS bash 3.2 an EXIT trap turns a set -u abort into exit 0, so this
# script could crash and report success. See scripts/lib/completion-guard.sh.
# shellcheck source=../../scripts/lib/completion-guard.sh
. "$ROOT_DIR/scripts/lib/completion-guard.sh"
arm_completion_guard "run-e2e" cleanup

# ---------------------------------------------------------------------------
# The parity table: route | cli subcommand | mcp tool
# ---------------------------------------------------------------------------
# Names differ where the best name differs per surface (`banshee status` reads
# better than `banshee pressure`), which is why this is a table of triples rather
# than an assertion that the three names match. Adding a route without its CLI
# subcommand and MCP tool fails the checks below.
PARITY_TABLE=(
  "/health|health|health"
  "/pressure|status|pressure"
  "/headroom|headroom|headroom"
  "/samples|samples|samples"
  "/rollups|rollups|rollups"
  "/census|census|census"
  "/alerts|alerts|alerts"
  "/deltas|deltas|deltas"
  "/stats|stats|stats"
)

# ALWAYS build. `cargo test` compiles test harnesses, not the shipping
# binaries, and an existing binary can be STALE — a stale green binary
# passing e2e while current source is broken is the failure mode that
# matters (bit a stamped project during mutation testing, 2026-08-19).
# A no-op build is nearly free.
echo "e2e: building workspace binaries..."
(cd "$ROOT_DIR/rust" && cargo build --workspace --quiet) || fail "cargo build failed"
for bin in banshee-api banshee banshee-mcp; do
  [ -x "$BIN/$bin" ] || fail "missing $BIN/$bin after cargo build"
done
command -v jq >/dev/null || fail "jq is required"

# ---------------------------------------------------------------------------
# Boot the API in the test profile on an ephemeral port
# ---------------------------------------------------------------------------
export BANSHEE_PROFILE=test
export BANSHEE_DB_PATH="$WORK/banshee.db"
export BANSHEE_PORT=0
export BANSHEE_KEY_FILE="$WORK/api_key"
# Fast samples so a verdict settles immediately; ONE census for the whole run so
# /census is frozen; a sweep far enough out that it never fires mid-run.
export BANSHEE_SAMPLE_INTERVAL_MS=120
export BANSHEE_CENSUS_INTERVAL_MS=600000
export BANSHEE_SWEEP_INTERVAL_MS=600000

# An IMPOSSIBLE census overlay, so the action routes (section 8) are provable
# no-ops on whatever machine runs this. `stale_days` beyond any real uptime
# means no session is ever a reap candidate; an orphan pattern nothing matches
# means no orphan is ever a kill target. So `execute` genuinely cannot touch a
# real process here — the harness exercises the ROUTING and the wire shape, not
# a live kill. (The rails themselves are pinned in banshee-core's unit tests,
# which is the right place: they must not depend on the host having, or not
# having, a reapable session.)
cat >"$WORK/census.local.toml" <<'TOML'
stale_days = 100000
orphan_patterns = ["zzzz-banshee-e2e-nonexistent-pattern-zzzz"]
TOML
export BANSHEE_CENSUS_OVERLAY="$WORK/census.local.toml"

"$BIN/banshee-api" >"$WORK/api.out" 2>"$WORK/api.err" &
API_PID=$!

# The daemon announces BOTH transports (ADR-0008), so each parse anchors on its own
# scheme. A bare `listening on` match returns two lines and yields a multi-line BASE —
# which is exactly how this harness broke when the socket was added.
BASE=""
SOCKET=""
for _ in $(seq 1 50); do
  BASE="$(sed -n 's|.*listening on \(http://[^ ]*\)|\1|p' "$WORK/api.out" | tail -1)"
  SOCKET="$(sed -n 's|.*listening on unix://||p' "$WORK/api.out" | tail -1)"
  [ -n "$BASE" ] && [ -n "$SOCKET" ] && break
  kill -0 "$API_PID" 2>/dev/null || { cat "$WORK/api.err" >&2; fail "API died on startup"; }
  sleep 0.1
done
[ -n "$BASE" ] || fail "API never announced its port"
[ -n "$SOCKET" ] || fail "API never announced its socket"

# The CLI and the MCP server go over the SOCKET, because that is what production uses
# and it is the only transport the file-loaded key travels over (ADR-0008).
# BANSHEE_API_URL is deliberately NOT exported: an explicit URL wins in every client's
# resolution order, which would put them back on TCP and — correctly — leave them
# unable to authenticate.
export BANSHEE_API_SOCKET="$SOCKET"
API_KEY="$(cat "$WORK/api_key")"
echo "e2e: API at $BASE and unix://$SOCKET"

# --- helpers ---------------------------------------------------------------
#
# The HTTP column of the parity table goes over the SOCKET (ADR-0008 Phase 3): the
# daemon refuses every credential on the TCP port, so a keyed curl there is not a
# read, it is the section-5 negative case. `--unix-socket` makes curl speak HTTP/1.1
# on the socket; the host in the URL is then a required placeholder, nothing more.
# $BASE (the TCP announcement) survives for the two things TCP is still for: the
# open /health and proving that the key is refused there.
SOCK="http://localhost"
scurl() { curl --unix-socket "$SOCKET" "$@"; }
http() { scurl -sf -H "x-api-key: $API_KEY" "$SOCK$1"; }
http_status() { scurl -s -o /dev/null -w '%{http_code}' -H "x-api-key: $API_KEY" "$SOCK$1"; }
http_post() { scurl -sf -X POST -H "x-api-key: $API_KEY" "$SOCK$1"; }
http_post_status() { scurl -s -o /dev/null -w '%{http_code}' -X POST -H "x-api-key: $API_KEY" "$SOCK$1"; }

# The MCP tool names, sorted. Honours BANSHEE_MCP_ENABLE_ACTIONS in the caller's
# environment, which is how section 8 proves the execute tools are absent when
# the gate is off and present when it is on.
mcp_tool_names() {
  printf '%s\n' '{"jsonrpc":"2.0","id":9,"method":"tools/list"}' \
    | "$BIN/banshee-mcp" | jq -r '.result.tools[].name' | sort
}

# One MCP tool call, returning the tool's result text.
#
# The request is built with jq rather than by string-splicing: `${2:-{\}}` leaves
# the backslash IN the default value under bash, so an empty argument object went
# out as `{\}`, the server replied with a parse error, and `.result.content[0]`
# came back null — which `jq -Sc '[paths]'` then reported as `[]`, a shape
# mismatch that looked like a wire-format bug rather than a quoting bug.
mcp() {
  local name="$1" args="${2:-null}"
  jq -nc --arg name "$name" --argjson args "$args" \
    '{jsonrpc:"2.0", id:1, method:"tools/call",
      params:{name:$name, arguments:($args // {})}}' \
    | "$BIN/banshee-mcp" | jq -r '.result.content[0].text'
}

# Every key PATH in a payload with ARRAY INDICES ABSTRACTED, sorted and deduped —
# the wire-format structure, without the float values the contract refuses to pin
# and without the array LENGTHS, which move while the harness is looking.
#
# Abstracting the indices is not laziness. The first version kept them and failed
# with `findings.4.*` present in the CLI's read and absent from the HTTP read
# taken 30ms earlier: a dimension had crossed a band mid-run and the worklist grew
# by one. That is the monitored machine doing its job, not a wire-format defect,
# and a gate that fails on it is a gate that gets disabled.
keyshape() {
  jq -Sc '[paths | map(if type == "number" then "[]" else . end) | join(".")]
          | unique | sort'
}

# --- 1. Every surface answers, and their key structure is identical ---------
#
# The census tier takes its first reading at startup by shelling out to ps/tmux,
# so poll for it rather than assuming it has landed. Same for the first sample.
echo "e2e: waiting for the first sample and census..."
for _ in $(seq 1 100); do
  ready=1
  [ "$(http /stats | jq -r .samples)" -ge 2 ] 2>/dev/null || ready=0
  [ "$(http_status /census)" = "200" ] || ready=0
  [ "$ready" = "1" ] && break
  sleep 0.1
done
[ "$(http_status /census)" = "200" ] || fail "no census after 10s; api.err:
$(cat "$WORK/api.err")"
SAMPLES="$(http /stats | jq -r .samples)"
[ "$SAMPLES" -ge 2 ] || fail "only $SAMPLES samples after 10s"
echo "e2e: $SAMPLES samples, 1 census"

for row in "${PARITY_TABLE[@]}"; do
  IFS='|' read -r route cli tool <<<"$row"

  view_http="$(http "$route" | keyshape)" || fail "HTTP $route failed"
  view_cli="$("$BIN/banshee" "$cli" --json 2>/dev/null | keyshape)" \
    || fail "CLI '$cli --json' failed"
  view_mcp="$(mcp "$tool" | keyshape)" || fail "MCP '$tool' failed"

  [ "$view_http" = "$view_cli" ] || fail "$route: HTTP and CLI '$cli' disagree in shape:
  http: $view_http
  cli:  $view_cli"
  [ "$view_http" = "$view_mcp" ] || fail "$route: HTTP and MCP '$tool' disagree in shape:
  http: $view_http
  mcp:  $view_mcp"
done
echo "e2e: all ${#PARITY_TABLE[@]} reads agree in shape across HTTP, CLI and MCP"

# --- 2. LITERAL byte parity on the FROZEN payloads -------------------------
#
# /census is taken once for the whole run and /health is constant, so these are
# compared as RAW BYTES — no jq normalization at all, not even key-sorting. That
# is only possible because both clients pass the server's bytes through instead of
# re-serializing a parsed value, and it is the strongest form of the guarantee:
# three surfaces, one answer, identical to the byte.
#
# It is also how this harness earned its keep. The first version normalized with
# `jq -Sc` and still failed, on a live census, with
# `"cpuSecsTotal":15640.710000000001` from HTTP against `15640.71` from the CLI:
# `serde_json` reads a float back one unit in the last place lower, so both
# clients were quietly altering every float they relayed. No amount of unit
# testing inside one process would have shown that.
#
# /census is the important row: deeply nested (orphans, tmuxSessions, appGroups,
# monitorAgents, monitorTotal), which is where casing bugs hide (Rule 3).
for row in "/census|census|census" "/health|health|health"; do
  IFS='|' read -r route cli tool <<<"$row"
  a="$(http "$route")"
  b="$("$BIN/banshee" "$cli" --json)"
  c="$(mcp "$tool")"
  [ "$a" = "$b" ] || fail "$route: CLI bytes differ from HTTP (floats?):
  http: $a
  cli:  $b"
  [ "$a" = "$c" ] || fail "$route: MCP bytes differ from HTTP (floats?):
  http: $a
  mcp:  $c"
done
echo "e2e: frozen payloads (/census, /health) are byte-identical, unnormalized"

# A closed time window is frozen too, however fast samples are landing — which is
# what makes byte parity on a LIST payload possible at all.
WINDOW_FROM="1970-01-01T00:00:00.000000Z"
WINDOW_TO="$(http /samples | jq -r '.[-1].sampledAt')"
[ -n "$WINDOW_TO" ] && [ "$WINDOW_TO" != "null" ] || fail "no sampledAt to window on"
Q="from=${WINDOW_FROM//:/%3A}&to=${WINDOW_TO//:/%3A}"
w_http="$(http "/samples?$Q")"
w_cli="$("$BIN/banshee" samples --from "$WINDOW_FROM" --to "$WINDOW_TO" --json)"
w_mcp="$(mcp samples "{\"from\":\"$WINDOW_FROM\",\"to\":\"$WINDOW_TO\"}")"
[ "$w_http" = "$w_cli" ] || fail "windowed /samples: CLI bytes differ from HTTP"
[ "$w_http" = "$w_mcp" ] || fail "windowed /samples: MCP bytes differ from HTTP"
[ "$(printf '%s' "$w_http" | jq 'length')" -ge 2 ] \
  || fail "the sample window must be non-empty for this comparison to mean anything"
echo "e2e: a closed sample window is byte-identical on all three"

# --- 3. The verdict itself agrees across surfaces --------------------------
#
# /pressure is the one payload that CANNOT be frozen: the sampler republishes a
# verdict on every sample, and this daemon samples every 120ms deliberately. So the
# comparison BRACKETS the three reads with two HTTP reads. If the bracketing reads
# are identical, the server state did not move and all four must match; if they
# differ, the machine changed under us and the round is retried.
#
# A genuine disagreement therefore always fails, and a moving machine never does —
# which is the property a gate needs. A tolerance ("compare only the discrete
# fields") would have hidden the float bug that section 2 exists to catch.
agreed=0
for _ in $(seq 1 20); do
  before="$(http /pressure)"
  v_cli="$("$BIN/banshee" status --json)"
  v_mcp="$(mcp pressure)"
  after="$(http /pressure)"
  [ "$before" = "$after" ] || continue   # the verdict moved; take another reading
  [ "$before" = "$v_cli" ] || fail "the verdict differs between HTTP and CLI:
  http: $before
  cli:  $v_cli"
  [ "$before" = "$v_mcp" ] || fail "the verdict differs between HTTP and MCP:
  http: $before
  mcp:  $v_mcp"
  agreed=1
  break
done
[ "$agreed" = "1" ] || fail "the verdict never held still long enough to compare \
across surfaces in 20 attempts — suspect a non-deterministic field, since \
`evaluate` is a pure function of stored history (ADR-0005)"
echo "e2e: the verdict is byte-identical across HTTP, CLI and MCP"

# --- 4. Wire-format spot checks on the raw HTTP view -----------------------
CENSUS="$(http /census)"
TAKEN="$(printf '%s' "$CENSUS" | jq -r .takenAt)"
case "$TAKEN" in
  *.??????Z) : ;;
  *) fail "census takenAt is not canonical 6-digit form: $TAKEN" ;;
esac
printf '%s' "$CENSUS" | jq -e 'has("tmuxAvailable")' >/dev/null \
  || fail "tmuxAvailable must be present: 'could not look' is not 'zero sessions'"
printf '%s' "$CENSUS" | jq -e '.monitorTotal | has("percentOfOneCore")' >/dev/null \
  || fail "camelCase must hold at depth in the census"

PRESSURE="$(http /pressure)"
printf '%s' "$PRESSURE" | jq -e 'has("source")' >/dev/null \
  || fail "source must be PRESENT (as null when there is none), never absent"
printf '%s' "$PRESSURE" \
  | jq -e '.dimensions | all(has("trendPerSec") and has("pending") and has("observationGapSecs"))' \
  >/dev/null || fail "nullable dimension fields must be present-as-null"

# THERMAL (schema v11): a live daemon records the kernel's thermal level on EVERY
# sample (null is reserved for pre-v11 rows), the two nullable keys are present on
# every sample either way, and the eleventh dimension is on the verdict.
SAMPLES_JSON="$(http /samples)"
printf '%s' "$SAMPLES_JSON" \
  | jq -e 'all(has("thermalPressureLevel") and has("cpuSpeedLimitPercent"))' >/dev/null \
  || fail "thermal keys must be present (as null when unrecorded) on every sample"
printf '%s' "$SAMPLES_JSON" \
  | jq -e 'all(.thermalPressureLevel | type == "number" and . >= 0 and . <= 4)' >/dev/null \
  || fail "a live daemon must record the kernel thermal level (0..4) on every sample"
printf '%s' "$PRESSURE" | jq -e '[.dimensions[] | select(.key == "thermal")] | length == 1' >/dev/null \
  || fail "the thermal dimension must be on the verdict"

# HELD TIME MAY NEVER EXCEED THE SPAN THE SAMPLES ACTUALLY COVER.
#
# This is the invariant that was violated on 2026-09-01: a daemon running for 45
# seconds reported "held 17m" on every out-of-band dimension, because `held_secs`
# counted straight across a 17-minute hole left by an earlier process. `held_secs`
# feeds `is_sustained_red`, so the verdict was a false Shrieking (banshee-s6s).
#
# The e2e database is a fresh temp file, so every sample belongs to this run and the
# span is knowable. jq's `fromdate` cannot read fractional seconds, hence the sub().
SPAN="$(http /samples | jq '
  if length < 2 then 0
  else ((.[-1].sampledAt | sub("\\.[0-9]+Z$"; "Z") | fromdate)
        - (.[0].sampledAt | sub("\\.[0-9]+Z$"; "Z") | fromdate))
  end')"
printf '%s' "$PRESSURE" | jq -e --argjson span "$SPAN" '
  .dimensions
  | map(select(.heldSecs > ($span + 2)))
  | length == 0' >/dev/null || fail "a dimension claims to have held longer than the
  samples span ($SPAN s) — held_secs is counting across a gap:
  $(printf '%s' "$PRESSURE" | jq -c --argjson span "$SPAN" \
      '[.dimensions[] | select(.heldSecs > ($span + 2))
        | {key, heldSecs, observationGapSecs}]')"
printf '%s' "$PRESSURE" | jq -e '[paths | map(tostring) | join(".")] | all(contains("_") | not)' \
  >/dev/null || fail "a snake_case key leaked into the verdict"

# The one lie the level scale exists to prevent: no samples must never read as
# calm. Holds whatever state the machine is in when this runs.
printf '%s' "$PRESSURE" \
  | jq -e 'if .sampleCount == 0 then .level == "checking" else true end' >/dev/null \
  || fail "a verdict with no samples must be 'checking', never a confident level"
printf '%s' "$PRESSURE" | jq -e '.glyph | length > 0' >/dev/null \
  || fail "the glyph travels over the wire and must never be empty (ADR-0005)"

# The glyph and the `source` field must agree: a suffix appears exactly when a
# source is named. They are two fields computed by two functions that each re-apply
# the same threshold, so they can drift apart — and a menu bar reading 😧 while the
# payload says "memory" is the exact class of disagreement ADR-0005 exists to
# prevent. This check also catches a STALE binary, which is how the contradiction
# was first seen: `target/debug` was old, the library was fine, and it took several
# minutes to tell those apart.
#
# The RECOVERING decoration (ADR-0009) is the one other thing that may lengthen a
# calm glyph: 🩹 (one code point) exactly when `recovering` is true and the level
# is quiet/stirring. So the face is 1 code point, plus 1 for the decoration, and
# more than that only with a source.
printf '%s' "$PRESSURE" \
  | jq -e 'if .source == null
           then (.glyph | length) == (if .recovering and (.level == "quiet" or .level == "stirring") then 2 else 1 end)
           else (.glyph | length) > 1 end' \
  >/dev/null || fail "glyph, source and recovering disagree: $(printf '%s' "$PRESSURE" | jq -c '{level, source, recovering, glyph, accessibilityLabel}')"
printf '%s' "$PRESSURE" \
  | jq -e 'if .source == null and (.recovering | not) then (.accessibilityLabel | contains(", ") | not)
           elif .source != null then (.accessibilityLabel | contains(", "))
           else true end' >/dev/null \
  || fail "the spoken label must name a source exactly when the glyph shows one"
# The episode fields ride the verdict (ADR-0009): the flags are booleans at both
# depths and `activity` has its three counts, on a fresh daemon all zero.
printf '%s' "$PRESSURE" \
  | jq -e '(.recovering | type) == "boolean"
           and (.dimensions | all((.recovering | type) == "boolean"))
           and (.activity | has("open") and has("lastHour") and has("lastDay"))
           and (.activity.lastHour <= .activity.lastDay)' >/dev/null \
  || fail "the verdict must carry recovering + activity: $(printf '%s' "$PRESSURE" | jq -c '{recovering, activity}')"

# The who-line (WS2a) rides every finding: `who` is always an array, `whoLine`
# is always present, and the line exists EXACTLY when the list is non-empty and
# then starts with `who: ` — so no surface ever prints a bare `who:` or drops a
# named culprit. The host's verdict may have no findings at all (a calm machine);
# `all` over an empty list is true, which is the honest outcome there.
printf '%s' "$PRESSURE" \
  | jq -e '.findings | all(
             (.who | type) == "array"
             and has("whoLine")
             and ((.who | length) > 0) == (.whoLine != null)
             and (.whoLine == null or (.whoLine | startswith("who: ")))
           )' >/dev/null \
  || fail "who/whoLine disagree on a finding: $(printf '%s' "$PRESSURE" | jq -c '[.findings[] | {dimension, who, whoLine}]')"

# HEADROOM (ADR-0010) is a DECISION, and its parts must agree with each other and
# with the verdict it was derived from. `shouldWait` is exactly "zero
# parallelism"; a retry is present exactly when waiting and an EXPLICIT null
# otherwise; `checking` is always a wait (headroom is never invented before data);
# the conditions are `Ready` then the six sources in a fixed order, each with a
# tri-state status, a reason token, and `since` present (null when Unknown). The
# level here is the SAME level as /pressure — the two are published from one
# evaluation — so the pair is bracketed the way section 3 brackets the verdict.
for _ in 1 2 3 4 5; do
  before="$(http /pressure | jq -r .level)"
  HEADROOM="$(http /headroom)"
  after="$(http /pressure | jq -r .level)"
  [ "$before" = "$after" ] && break
done
[ "$before" = "$after" ] || fail "the machine kept changing level; could not bracket /headroom"
[ "$(printf '%s' "$HEADROOM" | jq -r .level)" = "$before" ] \
  || fail "/headroom.level ($(printf '%s' "$HEADROOM" | jq -r .level)) disagrees with /pressure.level ($before)"
printf '%s' "$HEADROOM" | jq -e '
  has("shouldWait") and has("recommendedParallelism") and has("retryAfterSecs")
  and has("cores") and has("reason") and has("conditions")
  and (.shouldWait | type) == "boolean"
  and (.shouldWait == (.recommendedParallelism == 0))
  and (.shouldWait == (.retryAfterSecs != null))
  and (if .level == "checking" then .shouldWait else true end)
  and (.reason | length > 0)
  and ([.conditions[].type] == ["Ready","CpuPressure","MemoryPressure","ThermalPressure","DiskPressure","SprawlPressure","CorporatePressure"])
  and (.conditions | all(
        (.status == "True" or .status == "False" or .status == "Unknown")
        and (.reason | length > 0) and has("since")
        and ((.status == "Unknown") == (.since == null) or .type == "Ready")))
  ' >/dev/null || fail "headroom shape/invariants violated: $(printf '%s' "$HEADROOM" | jq -c '{level, shouldWait, recommendedParallelism, retryAfterSecs, cores, conditions: [.conditions[] | {type, status, reason}]}')"
# A live daemon has samples, so `cores` is known and Ready is True once out of checking.
printf '%s' "$HEADROOM" | jq -e 'if .level != "checking" then (.cores > 0 and .conditions[0].status == "True") else (.cores == null and .conditions[0].status == "False") end' >/dev/null \
  || fail "headroom cores/Ready disagree with the level: $(printf '%s' "$HEADROOM" | jq -c '{level, cores, ready: .conditions[0]}')"
printf '%s' "$HEADROOM" | jq -e '[paths | map(tostring) | join(".")] | all(contains("_") | not)' \
  >/dev/null || fail "a snake_case key leaked into /headroom"
echo "e2e: headroom is a decision whose parts agree ($(printf '%s' "$HEADROOM" | jq -r '"\(.level): shouldWait=\(.shouldWait) parallelism=\(.recommendedParallelism) retry=\(.retryAfterSecs)"'))"

# DELTAS (WS2d) is arithmetic over a hole made honest: the read exists to answer
# "what changed" WITHOUT pretending a gap it never watched across is continuity.
# So its honesty fields must always ride the wire, present-as-null when empty:
# `observationGapSecs` at the top AND on every dimension (a gap absorbed silently
# is the `held_secs` bug all over again, `banshee-s6s`), and a non-empty `summary`
# the surfaces can print verbatim. Consumers and the census pair travel together —
# both need two censuses in reach — so one is populated exactly when the other is;
# on this run the 10-minute census interval leaves a single census, so both are the
# empty branch and `census` is null (not absent). `lookback` defaults to `1h`, and
# `episodeId` is present-as-null on a clock-interval delta.
DELTAS="$(http /deltas)"
printf '%s' "$DELTAS" | jq -e '
  has("evaluatedAt") and has("lookback") and has("episodeId")
  and has("requestedLookbackSecs") and has("anchorAt") and has("actualLookbackSecs")
  and has("verdict") and has("dimensions") and has("consumers") and has("census")
  and has("observationGapSecs") and has("summary")
  and (.summary | length > 0)
  and (.lookback == "1h")
  and (.dimensions | type) == "array"
  and (.dimensions | all(has("observationGapSecs")))
  and (.consumers | type) == "array"
  and ((.consumers | length > 0) == (.census != null))
  and (if .verdict != null then (.verdict | has("thenLevel") and has("nowLevel") and has("detail")) else true end)
  ' >/dev/null || fail "delta honesty contract violated: $(printf '%s' "$DELTAS" | jq -c '{lookback, summary, observationGapSecs, dims: [.dimensions[] | {key, observationGapSecs}], consumers: (.consumers | length), census}')"
printf '%s' "$DELTAS" | jq -e '[paths | map(tostring) | join(".")] | all(contains("_") | not)' \
  >/dev/null || fail "a snake_case key leaked into /deltas"
echo "e2e: deltas answer what changed without absorbing a gap ($(printf '%s' "$DELTAS" | jq -r '"lookback=\(.lookback) gap=\(.observationGapSecs) dims=\(.dimensions | length)"'))"

# Empty arrays are `[]`, never null.
http /alerts | jq -e 'type == "array"' >/dev/null || fail "/alerts must be an array"
echo "e2e: wire-format spot checks hold"

# --- 5. Auth boundary: /health open, every read closed ---------------------
for row in "${PARITY_TABLE[@]}"; do
  IFS='|' read -r route _ _ <<<"$row"
  [ "$route" = "/health" ] && continue
  status="$(scurl -s -o /dev/null -w '%{http_code}' "$SOCK$route")"
  [ "$status" = "401" ] || fail "unauthenticated $route over the socket returned $status, want 401"
done
scurl -sf "$SOCK/health" | jq -e '.status == "ok"' >/dev/null || fail "/health not ok over the socket"
curl -sf "$BASE/health" | jq -e '.status == "ok"' >/dev/null || fail "/health not ok over TCP"
echo "e2e: auth boundary holds (/health open, ${#PARITY_TABLE[@]} reads keyed)"

# --- 5b. THE TRANSPORT DECIDES (ADR-0008 Phase 3) --------------------------
#
# This is the one place the pairing in main.rs is tested against the REAL binary:
# the router cannot tell how a request arrived, so `main` builds one flavour per
# listener, and swapping the two would put the key back on the squattable port with
# every unit test still green. Same key, same route, both transports — the port must
# refuse it by name and the socket must serve it. Reverting the arm in
# `routes::router`, or swapping the pairing in `main`, fails here.
for row in "${PARITY_TABLE[@]}"; do
  IFS='|' read -r route _ _ <<<"$row"
  [ "$route" = "/health" ] && continue
  status="$(curl -s -o /dev/null -w '%{http_code}' -H "x-api-key: $API_KEY" "$BASE$route")"
  [ "$status" = "401" ] || fail "a VALID key over TCP was accepted on $route ($status) — Phase 3 is not holding"
  status="$(http_status "$route")"
  [ "$status" = "200" ] || fail "the same key over the socket returned $status on $route, want 200"
done
TCP_REFUSAL="$(curl -s -H "x-api-key: $API_KEY" "$BASE/pressure" | jq -r '.error')"
printf '%s' "$TCP_REFUSAL" | grep -q 'not accepted over TCP' \
  || fail "the TCP refusal must name the transport, not the key; got: $TCP_REFUSAL"
# The kill routes are the reason the key must not travel over the port.
status="$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "x-api-key: $API_KEY" "$BASE/actions/reap-orphans/execute")"
[ "$status" = "401" ] || fail "a keyed execute over TCP returned $status, want 401"
echo "e2e: the key is refused over TCP and accepted over the socket (ADR-0008 Phase 3)"

# --- 6. Capability parity: the surfaces cover exactly the table ------------
#
# Each surface hand-maintains its own list; without this gate they drift silently
# — `health` shipped on HTTP+CLI but not MCP once (agentapi C2).
# The read tools come from the parity table; the two read-only action PREVIEW
# tools are always present too (the destructive execute tools are gated and are
# checked separately in section 9, with the gate deliberately OFF here).
EXPECTED_TOOLS="$( { printf '%s\n' "${PARITY_TABLE[@]}" | cut -d'|' -f3
  printf 'reap_stale_sessions_preview\nreap_orphans_preview\n'; } | sort)"
unset BANSHEE_MCP_ENABLE_ACTIONS
MCP_TOOLS="$(mcp_tool_names)"
[ "$MCP_TOOLS" = "$EXPECTED_TOOLS" ] || fail "MCP tool set drifted from the parity table:
  want: $(echo "$EXPECTED_TOOLS" | tr '\n' ' ')
  got:  $(echo "$MCP_TOOLS" | tr '\n' ' ')"

for row in "${PARITY_TABLE[@]}"; do
  IFS='|' read -r _ cli _ <<<"$row"
  "$BIN/banshee" "$cli" --help >/dev/null 2>&1 \
    || fail "CLI subcommand missing for parity: '$cli'"
done

# And no route exists that the table does not know about. A route the table has
# not heard of has no CLI subcommand and no MCP tool by definition, which is the
# drift this whole section exists to catch.
for route in /pressure /samples /rollups /census /alerts /stats /health; do
  printf '%s\n' "${PARITY_TABLE[@]}" | cut -d'|' -f1 | grep -qxF "$route" \
    || fail "route $route is not in the parity table"
done
echo "e2e: capability parity holds across HTTP, CLI, MCP"

# --- 6b. The /backup carve-out: HTTP + CLI, deliberately NOT an MCP tool ----
#
# /backup is the one route that breaks the "every route has an MCP tool" rule
# (banshee-3xh): snapshotting the DB is operator maintenance, never an agent's
# job. The divergence is TESTED both ways rather than left silent — the same
# discipline the reap-execute gate gets — so a future MCP tool named `backup`
# (or a lost CLI subcommand) fails here.
"$BIN/banshee" backup --help >/dev/null 2>&1 \
  || fail "the backup CLI subcommand is missing"
mcp_tool_names | grep -qxF backup \
  && fail "backup must NOT be an MCP tool (operator maintenance, not an agent surface)"
# It works end to end: a verified snapshot, reported with its path + row count,
# and the file is really there (temp DB, so the backups/ dir cleans up with it).
BACKUP_JSON="$("$BIN/banshee" backup --json)"
BACKUP_PATH="$(printf '%s' "$BACKUP_JSON" | jq -r '.path')"
printf '%s' "$BACKUP_JSON" | jq -e '.samples >= 0 and (.path | test("/backups/"))' >/dev/null \
  || fail "backup did not report a verified snapshot: $BACKUP_JSON"
[ -f "$BACKUP_PATH" ] || fail "backup reported $BACKUP_PATH but no file is there"
echo "e2e: /backup is HTTP+CLI only (no MCP tool) and writes a verified snapshot"

# --- 7. Error-shape contract on the agent-prone paths (agentapi C1) --------
#
# Mutation-proof: revert the ApiQuery wrapper and the bad-limit body becomes
# text/plain, so the MCP error text stops naming the real cause.
for probe in "/samples?limit=5000" "/samples?limit=nonsense" "/alerts?since=yesterday" \
             "/no-such-route"; do
  body="$(scurl -s -H "x-api-key: $API_KEY" "$SOCK$probe")"
  printf '%s' "$body" | jq -e 'has("error")' >/dev/null \
    || fail "$probe did not return the {error} shape; got: $body"
done

# Wrong method on a read route.
POST_BODY="$(scurl -s -X POST -H "x-api-key: $API_KEY" "$SOCK/pressure")"
printf '%s' "$POST_BODY" | jq -e 'has("error")' >/dev/null \
  || fail "POST /pressure did not return the {error} shape; got: $POST_BODY"

# The REAL reason must survive all the way to the agent, not become
# "invalid JSON from API".
MCP_ERRTEXT="$(printf '%s\n' '{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"samples","arguments":{"limit":5000}}}' \
  | "$BIN/banshee-mcp" | jq -r '.result.content[0].text')"
printf '%s' "$MCP_ERRTEXT" | grep -q '500' \
  || fail "MCP swallowed the real over-limit error; got: $MCP_ERRTEXT"
printf '%s\n' '{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"samples","arguments":{"limit":5000}}}' \
  | "$BIN/banshee-mcp" | jq -e '.result.isError == true' >/dev/null \
  || fail "an over-limit read must surface as an MCP error"

# The CLI maps 404 to exit code 3, and everything else 4xx to 1 — the distinction
# a script needs in order to tell "not yet" from "broken".
set +e
"$BIN/banshee" samples --limit 5000 --json >/dev/null 2>&1
rc=$?
set -e
[ "$rc" = "1" ] || fail "CLI exit code for a rejected request was $rc, want 1"
echo "e2e: error-shape contract holds through CLI and MCP"

# --- 8. ACTIONS: parity, the no-body write contract, and the MCP gate --------
#
# The impossible census overlay (set at the top) makes every action a no-op: no
# session is stale enough and no orphan pattern matches, so `candidates` is `[]`
# and `execute` cannot touch a real process. So this section proves ROUTING and
# WIRE SHAPE, not a live kill — the rails are pinned in banshee-core.
#
# The action PREVIEW pairs: route | cli subcommand | mcp tool. Preview is
# read-only and always present on all three surfaces; execute is a separate
# concern below (destructive, POST-only, MCP-gated).
ACTION_PREVIEWS=(
  "/actions/reap-stale-sessions/preview|reap-stale-sessions|reap_stale_sessions_preview"
  "/actions/reap-orphans/preview|reap-orphans|reap_orphans_preview"
)

for row in "${ACTION_PREVIEWS[@]}"; do
  IFS='|' read -r route cli tool <<<"$row"

  view_http="$(http_post "$route" | keyshape)" || fail "POST $route failed"
  view_cli="$("$BIN/banshee" "$cli" --json 2>/dev/null | keyshape)" \
    || fail "CLI '$cli --json' (preview) failed"
  view_mcp="$(mcp "$tool" | keyshape)" || fail "MCP '$tool' failed"

  [ "$view_http" = "$view_cli" ] || fail "$route: HTTP and CLI '$cli' disagree in shape:
  http: $view_http
  cli:  $view_cli"
  [ "$view_http" = "$view_mcp" ] || fail "$route: HTTP and MCP '$tool' disagree in shape:
  http: $view_http
  mcp:  $view_mcp"

  # The no-op proof: the impossible overlay must leave nothing to reap.
  http_post "$route" | jq -e '.candidates == [] and .executed == false' >/dev/null \
    || fail "$route preview must be an empty no-op under the impossible overlay:
  $(http_post "$route")"
done
echo "e2e: action previews agree across HTTP, CLI and MCP (and are no-ops here)"

# Execute works on HTTP and CLI and stays a no-op. `--execute` is the flag that
# turns the CLI preview into a kill; with the overlay it kills nothing.
for pair in "reap-stale-sessions|/actions/reap-stale-sessions/execute" \
            "reap-orphans|/actions/reap-orphans/execute"; do
  IFS='|' read -r cli route <<<"$pair"
  http_post "$route" | jq -e '.executed == true and .candidates == []' >/dev/null \
    || fail "$route execute must run and be a no-op here: $(http_post "$route")"
  "$BIN/banshee" "$cli" --execute --json | jq -e '.executed == true' >/dev/null \
    || fail "CLI '$cli --execute' did not report executed"
done
echo "e2e: action execute routes run and are no-ops under the impossible overlay"

# The write contract the first write routes brought back (docs/wire-format.md):
# a caller-supplied target list is a 422 in the {error} shape, not a silent
# ignore. This is the assertion that fails if the ApiJson wrapper is reverted.
BAD_BODY_STATUS="$(scurl -s -o /dev/null -w '%{http_code}' -X POST \
  -H "x-api-key: $API_KEY" -H 'content-type: application/json' \
  -d '{"pids":[1]}' "$SOCK/actions/reap-orphans/execute")"
[ "$BAD_BODY_STATUS" = "422" ] || fail "a target list on execute must be 422, got $BAD_BODY_STATUS"
scurl -s -X POST -H "x-api-key: $API_KEY" -H 'content-type: application/json' \
  -d '{"pids":[1]}' "$SOCK/actions/reap-orphans/execute" \
  | jq -e 'has("error") and (.error | contains("recomputed"))' >/dev/null \
  || fail "the 422 must wear the {error} shape and say why"

# A GET on an execute route is a 405 in the {error} shape — actions are POST.
GET_STATUS="$(http_status /actions/reap-orphans/execute)"
[ "$GET_STATUS" = "405" ] || fail "GET on an action route must be 405, got $GET_STATUS"

# Auth: every action route is keyed.
for route in /actions/reap-stale-sessions/preview /actions/reap-stale-sessions/execute \
             /actions/reap-orphans/preview /actions/reap-orphans/execute; do
  status="$(scurl -s -o /dev/null -w '%{http_code}' -X POST "$SOCK$route")"
  [ "$status" = "401" ] || fail "unauthenticated POST $route returned $status, want 401"
done
echo "e2e: action write contract holds (no-body ok, target list 422, POST-only, keyed)"

# THE MCP GATE: the destructive execute tools are ABSENT without
# BANSHEE_MCP_ENABLE_ACTIONS=1, and PRESENT with it — absent, not
# present-and-refusing, so an agent cannot be offered a kill it was never meant
# to have.
unset BANSHEE_MCP_ENABLE_ACTIONS
GATED_OFF="$(mcp_tool_names)"
printf '%s\n' "$GATED_OFF" | grep -q '_execute$' \
  && fail "execute tools must be ABSENT without the gate; got: $(echo "$GATED_OFF" | tr '\n' ' ')"
printf '%s\n' "$GATED_OFF" | grep -qx 'reap_stale_sessions_preview' \
  || fail "the read-only preview tool must be present even with actions gated off"

GATED_ON="$(BANSHEE_MCP_ENABLE_ACTIONS=1 mcp_tool_names)"
printf '%s\n' "$GATED_ON" | grep -qx 'reap_stale_sessions_execute' \
  || fail "execute tools must be PRESENT with the gate on; got: $(echo "$GATED_ON" | tr '\n' ' ')"
printf '%s\n' "$GATED_ON" | grep -qx 'reap_orphans_execute' \
  || fail "reap_orphans_execute must be present with the gate on"

# And calling execute while gated off is refused by NAME, not silently run — the
# gate holds on the CALL too, defending against a client naming an unlisted tool.
ERR="$(printf '%s\n' '{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"reap_orphans_execute","arguments":{}}}' \
  | "$BIN/banshee-mcp" | jq -r '.result.content[0].text')"
printf '%s' "$ERR" | grep -q 'disabled' \
  || fail "a gated-off execute call must be refused with a reason; got: $ERR"
echo "e2e: the MCP action gate holds (execute absent off, present on, refused on call off)"

# --- 9. A DAEMON RESTART MUST NOT MANUFACTURE A SUSTAINED RED (banshee-s6s) ---
#
# The production failure, reproduced end to end: stop the daemon, leave a hole
# larger than the gap tolerance, restart it on the SAME database, and confirm that
# the verdict does not treat the hole as continuous observation.
#
# This section exists because the invariant above could not fire without it — a
# fresh temp database has no gaps in it, so reverting `Series::held` left the whole
# harness green. Verified by doing exactly that. The bug was found in production for
# the same reason: nothing tested a restart.
echo "e2e: restarting the daemon to leave a gap in the series..."
kill "$API_PID" 2>/dev/null || true
wait "$API_PID" 2>/dev/null || true
API_PID=""

# The gap ceiling is 3× the sample cadence, and the cadence is rounded UP to whole
# seconds for the model, so 120ms becomes 1s and the ceiling is 3s. Sleep past it.
# A fixed sleep is right HERE — the thing being waited for is wall-clock elapsed
# time, which is the one case where polling has nothing to poll.
sleep 4

"$BIN/banshee-api" >>"$WORK/api.out" 2>>"$WORK/api.err" &
API_PID=$!

# THE LAST announcement wins. The test profile binds an EPHEMERAL port
# (BANSHEE_PORT=0), so the restarted daemon comes back on a DIFFERENT port and the
# old $BASE is stale — the first version of this section probed the dead port and
# reported "the daemon did not come back". The configured port is a request; only
# the announcement is the truth, and after a restart only the LATEST one is.
# (`DaemonService.lastAnnouncedURL` exists for exactly this, on the Swift side.)
for _ in $(seq 1 100); do
  NEW_BASE="$(sed -n 's|.*listening on \(http://[^ ]*\)|\1|p' "$WORK/api.out" | tail -1)"
  [ -n "$NEW_BASE" ] && [ "$NEW_BASE" != "$BASE" ] && break
  sleep 0.1
done
[ -n "${NEW_BASE:-}" ] && [ "$NEW_BASE" != "$BASE" ] \
  || fail "the restarted daemon never announced a new port; api.err:
$(tail -20 "$WORK/api.err")"
BASE="$NEW_BASE"
# The socket path is the SAME after a restart (it is derived from the DB path), and
# the file was deliberately left behind by the shutdown — so this wait also proves
# the stale-socket path in bind_socket: connect-probe, find nobody, unlink, rebind.
# BANSHEE_API_URL is still NOT exported: it would move the CLI back to TCP, where the
# daemon now refuses the key.
for _ in $(seq 1 100); do
  scurl -sf "$SOCK/health" >/dev/null 2>&1 && break
  sleep 0.1
done
scurl -sf "$SOCK/health" >/dev/null 2>&1 \
  || fail "the daemon did not come back on its socket after the restart; api.err:
$(tail -20 "$WORK/api.err")"

# Wait for enough samples to leave `Checking` behind, so the dimensions are populated.
for _ in $(seq 1 100); do
  [ "$(http /pressure | jq -r .sampleCount)" -ge 2 ] && break
  sleep 0.1
done

AFTER="$(http /pressure)"
GAPPED="$(printf '%s' "$AFTER" | jq '[.dimensions[] | select(.observationGapSecs != null)] | length')"
[ "${GAPPED:-0}" -ge 1 ] || fail "after a restart with a 4s hole, at least one
  dimension must REPORT a gap; got:
  $(printf '%s' "$AFTER" | jq -c '[.dimensions[] | {key, heldSecs, observationGapSecs}]')"

# And no SAMPLE-TIER dimension may claim to have held longer than the post-restart
# run. This is the assertion that fails when the fix is reverted.
#
# The census tier is excluded on purpose: its gap ceiling is 3× ITS cadence
# (600000ms here → 1800s), so two censuses nine seconds apart — one from before the
# restart, one taken immediately after — are continuous observation by the model's
# own rule, and `agents`/`orphans`/`corporate` legitimately report the span. Whether
# they appear at all is a race (the post-restart census runs off the reactor and
# may or may not land before this read), which is why this assertion was green in
# isolation and red under load with `heldSecs: 9` on exactly those three keys.
OVERCLAIM="$(printf '%s' "$AFTER" | jq '[.dimensions[]
  | select(.key != "agents" and .key != "orphans" and .key != "corporate")
  | select(.heldSecs > 3)] | length')"
[ "${OVERCLAIM:-0}" -eq 0 ] || fail "a dimension claims held time spanning the
  restart gap — held_secs is counting across it:
  $(printf '%s' "$AFTER" | jq -c '[.dimensions[] | select(.heldSecs > 3)
      | {key, heldSecs, observationGapSecs}]')"
echo "e2e: a restart is reported as a gap, not as sustained pressure"

echo "e2e: PASS"
finish 0
