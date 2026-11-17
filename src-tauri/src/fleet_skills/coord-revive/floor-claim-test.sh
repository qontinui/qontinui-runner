#!/usr/bin/env bash
# Self-test for /coord-revive's FLOOR CLAIM -- the stamped, load-gated form of
# "coord was unavailable" that plan
# 2026-09-03-capability-floor-claims-carry-their-probe makes the ONLY sanctioned
# one (dossier stale-capability-floor).
#
# WHAT IT ASSERTS. Every case runs the real cascade in a sandbox where every
# door is dead by construction, and reads the FLOOR-CLAIM: line the run prints:
#
#   (a) all doors dead + low load               -> verdict=FLOOR,   exit 1
#   (b) all doors dead + build processes present -> verdict=UNKNOWN, exit 5
#       (a fake `pgrep` answers three same-user rustc/cargo pids; the box's
#       real process table is never consulted, so the case is the same on a
#       box mid-build and on an idle CI runner)
#   (b2) all doors dead + a high 1-minute load    -> verdict=UNKNOWN, exit 5
#   (u)  all doors dead + one axis opted out      -> verdict=UNKNOWN, exit 1,
#        reason=axis-unprobed (#888's third terminal verdict; never FLOOR)
#   (b3) all doors dead + a SLOW /health          -> verdict=UNKNOWN, exit 5,
#       and runner_build= carries the buildId that same /health body served
#   (c) a LIVE stub door in the cwd's .mcp.json  -> verdict=LIVE,    exit 0
#   (d) /proc/loadavg unreadable, `uptime` absent -> verdict=UNKNOWN, exit 5,
#       reason=load-unreadable: an UNREAD load never renders as a low one
#   (e) every claim line above parses under the script's own exported
#       FLOOR_CLAIM_LINE_RE, read out of the subject rather than re-typed here
#   (f) the line prints WITHOUT --floor-claim too (the lint's binding is
#       reachable from every existing run), and the flag suppresses the prose
#   (g) `--floor-claim` with a verb is a usage error (exit 4), never a probe
#
# Phase 2 (the envelope arm):
#   (h) a LIVE door whose control read answers a 2xx with ZERO rows under every
#       known list key renders live=0 envelope=UNKNOWN(key not confirmed) and a
#       LIVE-BUT-EMPTY door row; the same door answering rows under `hits`
#       stays live=1 with no envelope token
#   (i) `call` prints a stderr advisory when a result parses to zero rows
#       under a known key, and stdout stays the raw result
#   (s) the origin sentinel itself, on synthetic text (dials nothing): a
#       production https host, an upper-case scheme, an IPv6 loopback
#       literal, the live runner's 127.0.0.1:9876 and a port that merely
#       starts with the dead port's digits are flagged; the two pins are not;
#       origin_verdict reaches each of its four answers; and check_origins
#       fails a run that names no origin or a non-pin one. The sentinel lives
#       in the shared helper (ISOLATION, below), so this is its control for
#       every suite that sources it
#
# THE ORIGIN SENTINEL rides every run of the subject -- each run_case case and
# each (i)/(i2) `call` -- and is the assertion that reads the isolation below
# instead of trusting it. It comes from the shared helper, whose header says
# what it reads, what it cannot see and why. Here its pins are $DEAD and
# $STUB, and the one case exempt from naming any origin is (g), the usage
# error.
#
# THE DISCHARGE (last section). The assertions above are shown to be able to
# FAIL: five mutations of a staged copy of coord-revive.sh -- the load gate
# deleted, exit 5 re-mapped to 1, the envelope arm dropped, `alerts` dropped
# from the python arm's key list, and L6 made to ignore its override -- must
# each redden a re-run of this suite. Without that, a suite whose fixtures
# never reach the gate would read green forever.
#
# ISOLATION, load-bearing rather than tidy. Every run of the subject is
# `env -i` with ONE explicit allow-list, PINNED_ENV: the shared list that
# scripts/lib/coord-revive-test-isolation.sh builds -- its header says what
# that list carries and what it keeps out, credentials and session ids among
# them -- plus this suite's own entries below. It replaced pinning by name, and
# the base suite's record shows why. Measured under a caller environment built
# to provoke it, the subject then followed an exported runner port to its
# /health reads, wrote the caller's timing file, guard-decision log and
# capability-negative ledger, sourced the caller's $BASH_ENV and obeyed the
# caller's curl configuration on every dial; and nothing cleared the caller's
# credentials, so an exported $COORD_DEVICE_JWT rode L6's credentialed GET
# (below). Why most of the shared pins matter is told in the header of
# approval-half-test.sh. This suite's own:
#   $PWD                       the directory the run cds into, passed on
#                             because env -i strips it: a bash started without
#                             one spells its directory the physical way, which
#                             under a TMPDIR reached through a symlink is not
#                             the path these cases assert on
#   $PATH                      run_case puts its $BIN stubs ahead of the
#                             caller's
#   $HOME, $USERPROFILE, $CLAUDE_CONFIG_DIR
#                             a throwaway home -- the real user store and
#                             settings are never read
#   $QONTINUI_ROOT             the sandbox root, so L2's sibling sweep probes
#                             nothing outside it. It carries one symlink,
#                             qontinui-claude-config/scripts -> this repo's
#                             scripts/, for the resolver's rung 3; a staged
#                             lone copy never reaches that rung, because its
#                             rungs 1 and 2 already land on $SANDBOX/scripts
#   $QONTINUI_RUNNER_URL       re-pointed per case at the stub or the dead port;
#                             it OVERRIDES the 9876 default for every runner
#                             read, including the frontend-state /health read
#                             the claim stamps runner_build= from (that
#                             override is what this suite's runner_build
#                             assertion found missing)
#   $QONTINUI_WEB_HTTP_URL     the shared list's dead port, for L6, the WEB
#                             host -- a second remote host with its own
#                             variable, which $COORD_HTTP_URL does not reach.
#                             It is named here for its history: until plan
#                             2026-11-13-ccfg-floor-claim-test-sends-real-requests-to-production-coord
#                             it was unpinned, and every case that reaches L6
#                             (a, b, b2, b3, b3-fast, u, d, d2, f, f2) sent an
#                             anonymous GET to production api.qontinui.io and,
#                             when the caller exported a JWT-shaped
#                             $COORD_DEVICE_JWT, a second GET carrying it as a
#                             bearer, while this header claimed hermeticity
#   $COORD_REVIVE_NO_MINT      set -- the in-process mint is a POST that on a
#                             real runner EVICTS a live peer's slot
#   $COORD_REVIVE_LOADAVG_FILE a fixture file; `pgrep`, `uptime` and `nproc`
#                             are shadowed on $PATH by stubs the case controls
# and, from run_case alone, $COORD_REVIVE_HEALTH_SLOW_MS, plus whatever a case
# names in CASE_ENV (case (u)'s $COORD_REVIVE_NO_BOOTSTRAP), which run_case and
# run_call both forward and then empty.
#
# Hermetic BY THOSE PINS, and only by them: every origin the subject can
# derive is the stub or the dead port -- the carried URLs, the .mcp.json files
# the cases write, and nothing inherited -- and nothing is written outside the
# sandbox. Every helper a run sources is this repo's: the subject finds
# scripts/ at its own fixed depth, and each staged lone copy -- the staging
# control and every mutant -- sits exactly three directories below $SANDBOX,
# so its resolver's first two rungs land on $SANDBOX/scripts. Staged one level
# shallower, those rungs had landed on $TMPDIR/scripts or $TMPDIR/../scripts,
# outside the sandbox, and an envelope.sh planted there was sourced 18 times
# by every copy that reached it while every assertion still passed. The (i)
# and (i2) `call` cases run the same way and dial only the
# stub door their own .mcp.json names: the call verb runs no cascade. (i3)
# runs no part of the subject that does I/O: it sources one JSON-reading
# function and feeds it stdin, under the same `env -i` allow-list and from an
# empty directory, so no caller's $BASH_ENV or cwd reaches that subject code
# either; and the suite's own python runs with -I. A rung that
# grows a new remote default needs a new pin in the shared list. The origin
# sentinel turns a missing pin into a red case instead of a silent pass, but
# it reads output after the fact: the run that finds the gap has ALREADY sent
# the traffic, once per case that reaches the rung, in the main run and again
# in each discharge re-run.

set -u

# -P resolves symlinks. Run through a workspace root's .claude symlink, a
# logical cd would climb ../../.. on the symlink's side and miss scripts/lib.
HERE="$(cd -P "$(dirname "$0")" && pwd)"
SCRIPT="${MC_SUBJECT:-$HERE/coord-revive.sh}"
REPO_ROOT="$(cd -P "$HERE/../../.." && pwd)"
PASS=0
FAIL=0
FAILED_CASES=()

[ -r "$SCRIPT" ] || { echo "FATAL: $SCRIPT not readable"; exit 1; }
# The pinned environment and the origin sentinel, shared with
# approval-half-test.sh. Without it the subject cannot be isolated, so the
# suite refuses rather than run it.
ISOLATION_LIB="$REPO_ROOT/scripts/lib/coord-revive-test-isolation.sh"
# shellcheck source=/dev/null
. "$ISOLATION_LIB" || { echo "FATAL: cannot source $ISOLATION_LIB (resolved from HERE=$HERE), so the subject cannot be isolated"; exit 1; }
# Every python this suite runs itself -- the stub door, the I3 probe and
# (i3)'s encoder, all stdlib-only -- runs with -I, isolated: a caller's
# PYTHON* variables (a PYTHONPATH sitecustomize, say) do not reach it, and
# neither the script's directory nor the cwd joins sys.path. Measured without
# it: a planted sitecustomize loaded 35 times in one full run.
PY="$(command -v python3 || command -v python || true)"
[ -n "$PY" ] || { echo "FATAL: no python3/python for the stub door"; exit 1; }

ok()  { PASS=$((PASS + 1)); echo "ok    $1"; }
bad() { FAIL=$((FAIL + 1)); FAILED_CASES+=("$1"); echo "FAIL  $1"; }
assert_eq() { # <label> <want> <got>
  if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (want '$2', got '$3')"; fi
}
assert_has() { # <label> <needle> <haystack>
  case "$3" in *"$2"*) ok "$1" ;; *) bad "$1 (missing '$2')" ;; esac
}
assert_lacks() { # <label> <needle> <haystack>
  case "$3" in *"$2"*) bad "$1 (found '$2')" ;; *) ok "$1" ;; esac
}

SANDBOX="$(mktemp -d)" || { echo "FATAL: mktemp -d failed"; exit 1; }
STUB_PID=""
# Links come off first, never recursed through: see the helper's make_link.
cleanup() {
  [ -n "$STUB_PID" ] && kill "$STUB_PID" 2>/dev/null
  isolation_unlink_all
  rm -rf "$SANDBOX"
}
trap cleanup EXIT

FAKE_HOME="$SANDBOX/home"; mkdir -p "$FAKE_HOME/.qontinui"
ROOT="$SANDBOX/root"; mkdir -p "$ROOT/qontinui-claude-config"
make_link "$ROOT/qontinui-claude-config/scripts" "$REPO_ROOT/scripts"
# Where a staged lone copy's resolver looks first: every copy sits exactly
# three directories below $SANDBOX (see the discharge), so its rungs 1 and 2
# land here. The helper's STAGED COPIES says what landed outside before.
make_link "$SANDBOX/scripts" "$REPO_ROOT/scripts"
BIN="$SANDBOX/bin"; mkdir -p "$BIN"
LOADAVG="$SANDBOX/loadavg"
# THE PINNED ENVIRONMENT -- with what run_case and run_call add per run, the
# WHOLE environment the subject sees: every run of it is `env -i` with this
# list (the header's ISOLATION says why each entry is here). The shared part,
# the dead port included, comes from the isolation helper; QONTINUI_RUNNER_URL
# is the dead port there, run_case re-points it per case, and a later `env`
# assignment wins.
isolation_init "$SANDBOX"
DEAD="$ISOLATION_DEAD"
PINNED_ENV+=(
  HOME="$FAKE_HOME" USERPROFILE="$FAKE_HOME" CLAUDE_CONFIG_DIR="$FAKE_HOME/cc"
  QONTINUI_ROOT="$ROOT" COORD_REVIVE_NO_MINT=1 COORD_REVIVE_LOADAVG_FILE="$LOADAVG"
)

# ---------------------------------------------------------------- the stubs
# pgrep: <mode file> says how many build pids to report. `pgrep -u <user> -x
# <pattern>` from the subject is answered without looking at any process.
PGREP_MODE="$SANDBOX/pgrep-mode"; echo 0 > "$PGREP_MODE"
cat > "$BIN/pgrep" <<EOF
#!/usr/bin/env bash
n="\$(cat "$PGREP_MODE" 2>/dev/null || echo 0)"
case "\$n" in
  fail) exit 3 ;;
  0) exit 1 ;;
esac
i=0; while [ "\$i" -lt "\$n" ]; do i=\$((i + 1)); echo "\$((4000 + i))"; done
exit 0
EOF
chmod +x "$BIN/pgrep"
# nproc: a fixed 8, so load/nproc is a number this file chose.
printf '#!/usr/bin/env bash\necho 8\n' > "$BIN/nproc"; chmod +x "$BIN/nproc"
# uptime: absent-by-default (exit 127 look-alike) so case (d) really has no
# fallback; a case that wants it re-points the stub.
printf '#!/usr/bin/env bash\nexit 1\n' > "$BIN/uptime"; chmod +x "$BIN/uptime"

# ---------------------------------------------------------------- the stub door
# One process, switched by a MODE file, serving BOTH the runner's /health and
# an MCP door at /coord-mcp keyed on X-Coord-Mcp-Proxy-Key: stub-nonce.
STUB_MODE="$SANDBOX/stub-mode"; echo live > "$STUB_MODE"
cat > "$SANDBOX/stub.py" <<'PYEOF'
import json, sys, time
from http.server import BaseHTTPRequestHandler, HTTPServer

MODE_FILE = sys.argv[1]
BUILD = "58414a05-1788118917383"


def mode():
    try:
        with open(MODE_FILE, encoding="utf-8") as fh:
            return fh.read().strip()
    except OSError:
        return "live"


def tool_text(obj):
    return {"content": [{"type": "text", "text": json.dumps(obj)}]}


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _send(self, code, payload):
        body = json.dumps(payload).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if not self.path.startswith("/health"):
            self._send(404, {"error": "not found"})
            return
        if mode() == "health-slow":
            time.sleep(1.2)
        self._send(200, {"frontendReady": True, "frontendState": "ready", "buildId": BUILD})

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n) if n else b"{}"
        if self.path != "/coord-mcp":
            self._send(404, {"error": "not found"})
            return
        if self.headers.get("X-Coord-Mcp-Proxy-Key") != "stub-nonce":
            self._send(401, {"code": "COORD_MCP_PROXY_UNAUTHORIZED"})
            return
        try:
            req = json.loads(raw.decode("utf-8"))
        except Exception:
            self._send(400, {"error": "bad json"})
            return
        m = mode()
        rid = req.get("id", 1)
        method = req.get("method")
        if method == "tools/list":
            tools = [] if m == "empty-tools" else [{"name": "coord_memory_search"}, {"name": "coord_query_identity"}]
            self._send(200, {"jsonrpc": "2.0", "id": rid, "result": {"tools": tools}})
            return
        if method == "tools/call":
            name = (req.get("params") or {}).get("name")
            if name == "coord_memory_search":
                if m == "empty-records":
                    self._send(200, {"jsonrpc": "2.0", "id": rid, "result": tool_text({"records": [], "count": 0})})
                else:
                    self._send(200, {"jsonrpc": "2.0", "id": rid, "result": tool_text({"hits": [{"id": "m1", "title": "DOSSIER x"}], "count": 1})})
                return
            if name == "coord_alert_queue":
                if m == "empty-records":
                    self._send(200, {"jsonrpc": "2.0", "id": rid, "result": tool_text({"alerts": [], "count": 0, "total_count": 0, "next_cursor": None})})
                else:
                    self._send(200, {"jsonrpc": "2.0", "id": rid, "result": tool_text({"alerts": [{"id": 7, "kind": "pr_merge_stuck"}], "count": 1, "total_count": 1, "next_cursor": None})})
                return
            self._send(200, {"jsonrpc": "2.0", "id": rid, "result": tool_text({"device_id": "stub"})})
            return
        self._send(200, {"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "no such method"}})


srv = HTTPServer(("127.0.0.1", 0), H)
print(srv.server_address[1], flush=True)
srv.serve_forever()
PYEOF
"$PY" -I "$SANDBOX/stub.py" "$STUB_MODE" > "$SANDBOX/port" 2>"$SANDBOX/stub.err" &
STUB_PID=$!
PORT=""
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  PORT="$(tr -d '[:space:]' < "$SANDBOX/port" 2>/dev/null)"
  [ -n "$PORT" ] && break
  sleep 0.2
done
[ -n "$PORT" ] || { echo "FATAL: the stub door never reported a port"; cat "$SANDBOX/stub.err"; exit 1; }
STUB="http://127.0.0.1:$PORT"

# ---------------------------------------------------------------- the origin sentinel
# The isolation helper's, run over every run's $OUT and $ERR (check_origins).
# Its pins here: the dead port, and the stub this suite serves.
ORIGIN_PINS+=("$STUB")
# no_origin_reason <case>: why that case may name no origin at all. Every case
# not listed asks at least one door, and a door names the address it dialled,
# so a run of it that names none means the sentinel checked nothing.
# shellcheck disable=SC2317  # called by the helper's origin_verdict, out of shellcheck's sight
no_origin_reason() {
  case "$1" in
    g) echo "a usage error exits before any door is asked" ;;
    *) return 1 ;;
  esac
}

# ---------------------------------------------------------------- the runner
# run_case <name> <runner-url> [args...] -> RC, OUT, ERR, CLAIM (the claim line)
# CASE_ENV holds any extra VAR=value the case's run needs -- under `env -i` a
# prefix on run_case would never reach the subject -- and is emptied after it.
CASE_N=0
CASE_ENV=()
run_case() {
  local name="$1" runner="$2"; shift 2
  CASE_N=$((CASE_N + 1))
  local cwd="$SANDBOX/case-$CASE_N"; mkdir -p "$cwd"
  CASE_CWD="$cwd"
  if [ "${WITH_DOOR:-0}" = "1" ]; then
    printf '{"mcpServers":{"coord-mcp":{"url":"%s/coord-mcp","headers":{"X-Coord-Mcp-Proxy-Key":"stub-nonce"}}}}\n' "$STUB" > "$cwd/.mcp.json"
  fi
  OUT="$(cd "$cwd" && env -i "${PINNED_ENV[@]}" PWD="$PWD" PATH="$BIN:$PATH" QONTINUI_RUNNER_URL="$runner" \
        COORD_REVIVE_HEALTH_SLOW_MS="${SLOW_MS:-5000}" ${CASE_ENV[@]+"${CASE_ENV[@]}"} \
        bash "$SCRIPT" "$@" 2>"$SANDBOX/err")"
  RC=$?
  CASE_ENV=()
  ERR="$(cat "$SANDBOX/err")"
  CLAIM="$(printf '%s\n' "$OUT" | grep -m1 '^FLOOR-CLAIM: ' || true)"
  CLAIMS_SEEN="${CLAIMS_SEEN:-}
$CLAIM"
  check_origins "$name" "$OUT" "$ERR"
}
# run_call <name> <tool> <json> -> RC, OUT, ERR: the `call` verb from
# $CALL_CWD, under the same PINNED_ENV, the same CASE_ENV handling and the same
# sentinel as run_case.
run_call() {
  local name="$1"; shift
  OUT="$(cd "$CALL_CWD" && env -i "${PINNED_ENV[@]}" PWD="$PWD" ${CASE_ENV[@]+"${CASE_ENV[@]}"} \
        bash "$SCRIPT" call "$@" 2>"$SANDBOX/err")"
  RC=$?
  CASE_ENV=()
  ERR="$(cat "$SANDBOX/err")"
  check_origins "$name" "$OUT" "$ERR"
}
# The FIRST `key=` token on the claim line: the reason= parenthetical repeats
# load= / build_procs= / health_ms= after it, so a greedy match would read the
# echo, not the field.
field() { printf '%s\n' "$CLAIM" | tr ' ' '\n' | sed -n "s/^$1=//p" | head -n 1; }
low_load()  { printf '0.10 0.20 0.30 1/100 1\n' > "$LOADAVG"; }
high_load() { printf '64.00 60.00 55.00 9/100 1\n' > "$LOADAVG"; }

# ================================================================ (a) FLOOR
echo "== (a) every door dead, box idle -> FLOOR, exit 1"
low_load; echo 0 > "$PGREP_MODE"
run_case a "$DEAD" --floor-claim
assert_eq  "(a) exit 1"                          "1" "$RC"
assert_eq  "(a) verdict=FLOOR"                   "FLOOR" "$(field verdict)"
assert_eq  "(a) live=0"                          "0" "$(field live)"
assert_eq  "(a) build_procs=0 from the stub"     "0" "$(field build_procs)"
assert_eq  "(a) load=0.10/8 from the fixtures"   "0.10/8" "$(field load)"
assert_has "(a) runner_build is the honest UNKNOWN arm when /health never answered" "runner_build=UNKNOWN — unrecorded" "$CLAIM"
assert_eq  "(a) health_ms=UNKNOWN (refused connect is not a timeout)" "UNKNOWN" "$(field health_ms)"
assert_lacks "(a) no reason= on a FLOOR"         " reason=" "$CLAIM"
assert_has "(a) the door table names the L1 file" "  L1 $CASE_CWD/.mcp.json -> " "$OUT"
assert_has "(a) the door table names the L4 mint origin" "  L4 mint@$DEAD" "$OUT"
assert_lacks "(a) --floor-claim suppresses the DEAD list prose" "  - L1" "$OUT"
assert_has "(a) VERDICT: DEAD still leads the block"  "VERDICT: DEAD" "$OUT"

# ================================================================ (b) build procs
echo "== (b) every door dead, 3 same-user build processes -> UNKNOWN, exit 5"
low_load; echo 3 > "$PGREP_MODE"
run_case b "$DEAD" --floor-claim
assert_eq  "(b) exit 5"                           "5" "$RC"
assert_eq  "(b) verdict=UNKNOWN"                  "UNKNOWN" "$(field verdict)"
assert_eq  "(b) build_procs=3 (the fake pgrep's answer)" "3" "$(field build_procs)"
assert_has "(b) reason=sampled-under-own-load"    " reason=sampled-under-own-load (load=0.10/8, build_procs=3, health_ms=UNKNOWN)" "$CLAIM"
assert_has "(b) the line forbids writing unavailable from this sample" 'do NOT write "unavailable" from this sample' "$CLAIM"
echo 0 > "$PGREP_MODE"

echo "== (b2) every door dead, load 64 over 8 cores -> UNKNOWN, exit 5"
high_load
run_case b2 "$DEAD" --floor-claim
assert_eq  "(b2) exit 5"                          "5" "$RC"
assert_eq  "(b2) verdict=UNKNOWN"                 "UNKNOWN" "$(field verdict)"
assert_eq  "(b2) load=64.00/8"                    "64.00/8" "$(field load)"
assert_has "(b2) reason names the load"           " reason=sampled-under-own-load (load=64.00/8," "$CLAIM"
low_load

echo "== (b3) every door dead, /health answers SLOWLY -> UNKNOWN, exit 5; runner_build stamped"
echo health-slow > "$STUB_MODE"
SLOW_MS=200 run_case b3 "$STUB" --floor-claim
echo live > "$STUB_MODE"
assert_eq  "(b3) exit 5"                          "5" "$RC"
assert_eq  "(b3) verdict=UNKNOWN"                 "UNKNOWN" "$(field verdict)"
assert_eq  "(b3) runner_build= is the buildId /health served" "58414a05-1788118917383" "$(field runner_build)"
HM="$(field health_ms)"
case "$HM" in
  ''|*[!0-9]*) bad "(b3) health_ms is numeric (got '$HM')" ;;
  *) if [ "$HM" -gt 200 ]; then ok "(b3) health_ms=$HM exceeds the 200ms slow threshold"; else bad "(b3) health_ms=$HM did not measure a 1.2s /health"; fi ;;
esac
assert_has "(b3) reason cites health_ms"          "health_ms=$HM)" "$CLAIM"
# The same stub, answering promptly, is not a load signal: the build is still
# stamped, and with every door dead the verdict is FLOOR.
run_case b3-fast "$STUB" --floor-claim
assert_eq  "(b3) a prompt /health with every door dead -> FLOOR" "FLOOR" "$(field verdict)"
assert_eq  "(b3) ...still stamped with the runner build" "58414a05-1788118917383" "$(field runner_build)"

# ================================================================ (c) LIVE
echo "== (c) a LIVE stub door in the cwd's .mcp.json -> LIVE, exit 0"
WITH_DOOR=1 run_case c "$STUB" --floor-claim
assert_eq  "(c) exit 0"                           "0" "$RC"
assert_eq  "(c) verdict=LIVE"                     "LIVE" "$(field verdict)"
assert_eq  "(c) live=1"                           "1" "$(field live)"
assert_has "(c) VERDICT: LIVE leads the block"    "VERDICT: LIVE door=" "$OUT"
assert_has "(c) the door table carries the LIVE row with its url" "  LIVE $CASE_CWD/.mcp.json -> LIVE ($STUB/coord-mcp)" "$OUT"
assert_lacks "(c) no envelope token when the control read confirmed rows" " envelope=" "$CLAIM"
assert_lacks "(c) --floor-claim suppresses the NOTE prose" "NOTE: a LIVE door here does NOT restore" "$OUT"
assert_lacks "(c) no reason= on LIVE"             " reason=" "$CLAIM"

# ================================================================ (d) unreadable load
echo "== (u) every door dead but an axis opted out -> VERDICT: UNKNOWN, claim UNKNOWN reason=axis-unprobed, exit 1 (never FLOOR)"
CASE_ENV=(COORD_REVIVE_NO_BOOTSTRAP=1)
run_case u "$DEAD" --floor-claim
assert_eq  "(u) exit 1 (an unprobed axis is not the under-load arm)" "1" "$RC"
assert_eq  "(u) verdict=UNKNOWN"                  "UNKNOWN" "$(field verdict)"
assert_has "(u) reason names the axis"            " reason=axis-unprobed (unprobed=" "$CLAIM"
assert_has "(u) the line forbids writing unavailable from this sample" 'do NOT write "unavailable" from this sample' "$CLAIM"
assert_has "(u) VERDICT: UNKNOWN leads the block, not DEAD" "VERDICT: UNKNOWN - no door answered among the axes this run asked" "$OUT"
assert_lacks "(u) no FLOOR from an idle box while an axis is unprobed" "verdict=FLOOR" "$CLAIM"
assert_has "(u) the axes roster rides under the pasted block" "axes: hosts=127.0.0.1" "$OUT"

echo "== (d) /proc/loadavg unreadable and no uptime -> UNKNOWN, exit 5, load-unreadable"
rm -f "$LOADAVG"
run_case d "$DEAD" --floor-claim
low_load
assert_eq  "(d) exit 5"                           "5" "$RC"
assert_eq  "(d) verdict=UNKNOWN"                  "UNKNOWN" "$(field verdict)"
assert_eq  "(d) load=UNKNOWN/8"                   "UNKNOWN/8" "$(field load)"
assert_has "(d) reason=load-unreadable"           " reason=load-unreadable (" "$CLAIM"

echo "== (d2) pgrep failing (exit 3) is UNKNOWN, not zero processes"
echo fail > "$PGREP_MODE"
run_case d2 "$DEAD" --floor-claim
echo 0 > "$PGREP_MODE"
assert_eq  "(d2) exit 5"                          "5" "$RC"
assert_eq  "(d2) build_procs=UNKNOWN"             "UNKNOWN" "$(field build_procs)"
assert_has "(d2) reason=build-procs-unreadable"   " reason=build-procs-unreadable (" "$CLAIM"

# ================================================================ (e) the regex
echo "== (e) every claim line parses under the script's own FLOOR_CLAIM_LINE_RE"
RE="$(sed -n "s/^FLOOR_CLAIM_LINE_RE='\(.*\)'\$/\1/p" "$SCRIPT" | head -n 1)"
if [ -z "$RE" ]; then
  bad "(e) FLOOR_CLAIM_LINE_RE not found in $SCRIPT"
else
  ok "(e) FLOOR_CLAIM_LINE_RE read out of the subject"
  N_LINES=0; N_OK=0
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    N_LINES=$((N_LINES + 1))
    if printf '%s\n' "$line" | grep -E -q "$RE"; then N_OK=$((N_OK + 1)); else echo "      does not parse: $line"; fi
  done <<EOF
$CLAIMS_SEEN
EOF
  if [ "$N_LINES" -ge 8 ] && [ "$N_OK" -eq "$N_LINES" ]; then
    ok "(e) $N_OK of $N_LINES claim lines parse"
  else
    bad "(e) $N_OK of $N_LINES claim lines parse (need all, and at least 8)"
  fi
  if printf '%s\n' "FLOOR-CLAIM: verdict=FLOOR probed_at=today runner_build=x load=1/1 build_procs=0 health_ms=1 doors=0 live=0" | grep -E -q "$RE"; then
    bad "(e) the regex accepted a claim with no ISO probe time"
  else
    ok "(e) the regex refuses a claim with no ISO probe time (negative control)"
  fi
fi

# ================================================================ (f) without the flag
echo "== (f) the claim line prints on a plain run too, with the prose"
run_case f "$DEAD"
assert_eq  "(f) plain run, every door dead, idle -> exit 1" "1" "$RC"
assert_eq  "(f) the FLOOR-CLAIM line is present without --floor-claim" "FLOOR" "$(field verdict)"
assert_has "(f) the DEAD list prose is kept on a plain run" "  - L1 " "$OUT"
assert_has "(f) the SCOPE line is kept on a plain run" "SCOPE: the verdict above is exactly as wide as the AXES table" "$OUT"
echo 3 > "$PGREP_MODE"
run_case f2 "$DEAD"
echo 0 > "$PGREP_MODE"
assert_eq  "(f) a plain run under load also exits 5"  "5" "$RC"

# ================================================================ (g) usage
echo "== (g) --floor-claim takes no verb"
run_case g "$DEAD" --floor-claim call coord_orient
assert_eq  "(g) exit 4"                            "4" "$RC"
assert_has "(g) names the misuse"                  "takes no verb" "$ERR"
assert_eq  "(g) nothing was probed (no claim line)" "" "$CLAIM"

# ================================================================ (h) envelope
echo "== (h) a LIVE door whose control read parses to zero rows -> live=0 envelope=UNKNOWN"
echo empty-records > "$STUB_MODE"
WITH_DOOR=1 run_case h "$STUB" --floor-claim
echo live > "$STUB_MODE"
assert_eq  "(h) still exit 0 (the door is live; the KEY is unconfirmed)" "0" "$RC"
assert_eq  "(h) verdict=LIVE"                      "LIVE" "$(field verdict)"
assert_eq  "(h) live=0"                            "0" "$(field live)"
assert_has "(h) envelope=UNKNOWN(key not confirmed)" " envelope=UNKNOWN(key not confirmed)" "$CLAIM"
assert_has "(h) the door row says LIVE-BUT-EMPTY"   " -> LIVE-BUT-EMPTY (" "$OUT"
echo empty-tools > "$STUB_MODE"
WITH_DOOR=1 run_case h2 "$STUB" --floor-claim
echo live > "$STUB_MODE"
assert_eq  "(h2) tools/list with zero tools -> live=0" "0" "$(field live)"
assert_has "(h2) envelope=UNKNOWN on an empty tools/list" " envelope=UNKNOWN(key not confirmed)" "$CLAIM"

# ================================================================ (i) call advisory
echo "== (i) the call verb's stderr advisory on a zero-row result"
echo empty-records > "$STUB_MODE"
CALL_CWD="$SANDBOX/call"; mkdir -p "$CALL_CWD"
printf '{"mcpServers":{"coord-mcp":{"url":"%s/coord-mcp","headers":{"X-Coord-Mcp-Proxy-Key":"stub-nonce"}}}}\n' "$STUB" > "$CALL_CWD/.mcp.json"
run_call i-empty coord_memory_search '{"query":"x"}'
assert_eq  "(i) call exits 0 (the tool answered)"   "0" "$RC"
assert_has "(i) stdout is still the raw result"      'records' "$OUT"
assert_has "(i) stderr carries the envelope advisory" "0 rows under known list keys" "$ERR"
assert_has "(i) ...and names the UNKNOWN(envelope) arm" "UNKNOWN(envelope) until confirmed" "$ERR"
echo live > "$STUB_MODE"
run_call i-rows coord_memory_search '{"query":"x"}'
assert_lacks "(i) no advisory when rows are present"  "0 rows under known list keys" "$ERR"

echo "== (i2) the agent alert queue's rows key (alerts) is a known list key"
echo empty-records > "$STUB_MODE"
run_call i2-empty coord_alert_queue '{"domain":"cleanup"}'
assert_eq  "(i2) call exits 0 (the tool answered)"         "0" "$RC"
assert_has "(i2) an empty queue carries the advisory"      "0 rows under known list keys" "$ERR"
echo live > "$STUB_MODE"
run_call i2-rows coord_alert_queue '{"domain":"cleanup"}'
assert_lacks "(i2) no advisory over a populated queue"     "0 rows under known list keys" "$ERR"

echo "== (i3) the PYTHON arm of rpc_result_rows counts the alerts key too"
# (i2) runs whichever arm the box selects -- jq wherever jq is installed -- so
# a regression confined to the python arm would pass it. Drive that arm
# directly: lift rpc_result_rows out of the SUBJECT (so a mutant is what is
# measured) and run it with JSON_READER pinned to a working python, under
# `env -i` like every other run of subject code. Measured without that, the
# rest of this suite's isolation in place: a caller's $BASH_ENV was sourced
# into the bash -c, and all three (i3) assertions still passed.
I3_PY=""
for c in python3 python; do
  if command -v "$c" >/dev/null 2>&1 \
     && [ "$("$c" -I -c 'import json;print(1)' </dev/null 2>/dev/null | tr -d '\r\n')" = "1" ]; then
    I3_PY="$c"; break
  fi
done
sed -n '/^rpc_result_rows() {$/,/^}$/p' "$SCRIPT" > "$SANDBOX/rows-fn.sh"
if [ -z "$I3_PY" ]; then
  bad "(i3) no working python to pin the python arm to"
elif [ ! -s "$SANDBOX/rows-fn.sh" ]; then
  bad "(i3) rpc_result_rows() not found in the subject"
else
  # The subject's python runs from an empty directory of the sandbox's: a
  # `python -c` puts its cwd first on sys.path, and the caller's cwd is not
  # this suite's to vouch for.
  mkdir -p "$SANDBOX/i3"
  i3_rows() { # <tool JSON>
    printf '%s' "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":$("$I3_PY" -I -c 'import json,sys;print(json.dumps(sys.argv[1]))' "$1" | tr -d '\r')}]}}" \
      | (cd "$SANDBOX/i3" && env -i "${PINNED_ENV[@]}" PWD="$PWD" JSON_READER="$I3_PY" \
           bash -c ". '$SANDBOX/rows-fn.sh'; rpc_result_rows") | tr -d '\r'
  }
  assert_eq "(i3) python arm: an empty alerts page counts 0"   "0"  "$(i3_rows '{"alerts":[],"count":0}')"
  assert_eq "(i3) python arm: a populated alerts page counts 1" "1"  "$(i3_rows '{"alerts":[{"id":7}],"count":1}')"
  assert_eq "(i3) python arm: no known key is -1, never 0"      "-1" "$(i3_rows '{"rows":[]}')"
fi

# ================================================================ (s) the sentinel itself
echo "== (s) the origin sentinel flags every origin that is not a pin, and only those (dials nothing)"
# Synthetic lines in the shapes the cascade prints, so the sentinel is shown
# to be able to FAIL a run: the cases above only ever show it passing one.
# coord.qontinui.io appears ONLY in upper case, so finding it flagged proves
# the upper-case scheme and host were read rather than skipped.
S_TEXT="  L6 web-host@https://api.qontinui.io -> WEB_HOST_UNREACHABLE
L6: the anonymous GET https://api.qontinui.io/api/v1/plan-library?kind=plan&limit=1 never completed
  L5: the mint POST to HTTPS://COORD.QONTINUI.IO/agents/credential never completed
  L4 mint@http://127.0.0.1:9876 source=runner-invoke:get_coord_device_token -> NO_RUNNER
  L4 mint@http://[::1]:9876 source=runner-invoke:get_coord_device_token -> NO_RUNNER
  L4 mint@http://127.0.0.1:10 source=runner-eval -> NO_RUNNER
  L4 device-jwt@\$COORD_DEVICE_JWT -> UNREACHABLE ($DEAD/mcp)
  LIVE $SANDBOX/s/.mcp.json -> LIVE ($STUB/coord-mcp)"
S_OFF="$(offbox_origins "$S_TEXT")"
assert_flagged() { # <label> <origin> <yes|no>
  local got=no
  printf '%s\n' "$S_OFF" | grep -qxF -e "$2" && got=yes
  assert_eq "$1" "$3" "$got"
}
assert_eq "(s) seven distinct origins are read, the two pins among them" \
  "7" "$(origins_named "$S_TEXT" | grep -c .)"
assert_flagged "(s) a production https host is flagged"                    "https://api.qontinui.io" yes
assert_flagged "(s) an upper-case scheme and host are read, lower-cased and flagged" "https://coord.qontinui.io" yes
assert_flagged "(s) the live runner's loopback port is flagged"             "http://127.0.0.1:9876" yes
assert_flagged "(s) an IPv6 loopback literal is flagged"                    "http://[::1]:9876" yes
assert_flagged "(s) a port that only starts with the dead port's digits is flagged" "http://127.0.0.1:10" yes
assert_flagged "(s) the dead-port pin is not flagged"                       "$DEAD" no
assert_flagged "(s) the stub pin is not flagged"                            "$STUB" no
# No pins is no allow-list. Emptied in a subshell, so the suite's own pins
# survive: every origin the text names must then be flagged.
assert_eq "(s) with no pins at all, every origin is flagged" \
  "$(origins_named "$S_TEXT")" "$(ORIGIN_PINS=(); offbox_origins "$S_TEXT")"
# The decision itself, one input per answer -- including the vacuity arm,
# which no real case reaches while the cascade keeps printing its addresses.
assert_eq "(s) origin_verdict: a non-pin origin is offbox"          "offbox" "$(origin_verdict x "$S_TEXT")"
assert_eq "(s) origin_verdict: a non-pin origin outranks an exempt case" "offbox" "$(origin_verdict g "$S_TEXT")"
assert_eq "(s) origin_verdict: only pins named is seen"             "seen"   "$(origin_verdict x "($DEAD/mcp) ($STUB/coord-mcp)")"
assert_eq "(s) origin_verdict: no origin from an exempt case is exempt" "exempt" "$(origin_verdict g "")"
assert_eq "(s) origin_verdict: no origin from any other case is none"   "none"   "$(origin_verdict x "")"
# And check_origins itself, over each answer. It runs in a subshell, so the
# line it prints is captured and the counts it bumps are thrown away. The
# vacuity arm needs this most: no real case reaches it, so turning its catch-all
# into an ok would otherwise leave every assertion green.
s_check() { ( check_origins "$1" "$2" ); } # <case> <run output>
assert_has "(s) check_origins FAILS a run that names no origin"        "FAIL  (x) the run named no origin at all" "$(s_check x "")"
assert_has "(s) check_origins FAILS a run naming a non-pin origin"     "FAIL  (x) the run named an origin that is not one of the suite's pins" "$(s_check x "$S_TEXT")"
assert_has "(s) check_origins passes a run naming only pins"           "ok    (x) every origin the run named is a pin" "$(s_check x "$DEAD/mcp")"
assert_has "(s) check_origins passes an exempt case that names none"   "ok    (g) the run named no origin, as it should" "$(s_check g "")"

# ================================================================ the discharge
if [ "${MC_MUTANT:-0}" = "1" ]; then
  :   # a re-run must not drive its own mutations
else
  MC_LIB="$REPO_ROOT/scripts/lib/mutation-control.sh"
  echo "== discharge: the assertions above are shown to be able to fail"
  # FATAL rather than a skip, as in approval-half-test.sh: a run that cannot
  # discharge must not read as one that did.
  # shellcheck source=/dev/null
  . "$MC_LIB" || { echo "FATAL: cannot source $MC_LIB (resolved from HERE=$HERE), so the discharge cannot run"; exit 1; }
  mc_ok()  { ok "$*"; }
  mc_bad() { bad "$*"; }
  # Staged under $SANDBOX/stage, so every mutant is
  # $SANDBOX/stage/mutation-control/mN/coord-revive.sh: three directories
  # below $SANDBOX, where its resolver's first two rungs find $SANDBOX/scripts.
  mc_init ".claude/skills/coord-revive/floor-claim-test.sh" "$SANDBOX/stage"
  # The staging control -- an unmutated copy in the mutants' own layout,
  # which must run green, and its rung-1 check -- lives in the shared helper.
  isolation_staging_control "$SCRIPT" "$REPO_ROOT" -- bash "$0"
  mc_expect_red "delete the load gate, so a sample under load renders FLOOR" \
    "$SCRIPT" '/^      reason="\$(floor_gate_reason)"$/d' \
    -- bash "$0"
  mc_expect_red "map UNKNOWN-under-load to exit 1, so it reads as DEAD" \
    "$SCRIPT" 's/verdict="UNKNOWN"; FLOOR_EXIT=5/verdict="UNKNOWN"; FLOOR_EXIT=1/' \
    -- bash "$0"
  # The arm is REPLACED by a no-op rather than deleted: deleting it leaves an
  # `if ... then` whose body is only comments, so the mutated copy does not
  # parse and reddens every assertion for a reason that has nothing to do with
  # the envelope (scripts/lib/mutation-control.sh's parse gate now reports
  # that as vacuous rather than counting it as a kill).
  mc_expect_red "drop the envelope arm, so an empty control read stays live=1" \
    "$SCRIPT" 's/^        live=0; tail=" envelope=UNKNOWN(key not confirmed)"$/        :/' \
    -- bash "$0"
  mc_expect_red "drop alerts from the PYTHON arm's key list only, so (i3) must catch it" \
    "$SCRIPT" 's/^KEYS=("hits","records","results","items","findings","documents","work_units","gates","tools","alerts")$/KEYS=("hits","records","results","items","findings","documents","work_units","gates","tools")/' \
    -- bash "$0"
  # The origin sentinel's own discharge. L6 is re-pointed at ANOTHER loopback
  # address rather than back at its remote default, so the re-run that must
  # redden sends nothing off the box either.
  mc_expect_red "L6 ignores its override, so the web host is not the pinned dead port" \
    "$SCRIPT" 's|^WEB_URL="\${QONTINUI_WEB_HTTP_URL:-https://api.qontinui.io}"$|WEB_URL="http://127.0.0.2:1"|' \
    -- bash "$0"
  mc_trailer "$((PASS + FAIL))"
fi

echo "--------------------------------------------------------------"
if [ "$FAIL" -eq 0 ]; then
  echo "PASS  $PASS assertions, 0 failures"
  exit 0
fi
echo "$FAIL of $((PASS + FAIL)) assertions FAILED:"
for f in "${FAILED_CASES[@]}"; do echo "  - $f"; done
exit 1
