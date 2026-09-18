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
#
# THE DISCHARGE (last section). The assertions above are shown to be able to
# FAIL: a staged copy of coord-revive.sh with the load gate deleted, and one
# with exit 5 re-mapped to 1, must each redden a re-run of this suite. Without
# that, a suite whose fixtures never reach the gate would read green forever.
#
# ISOLATION, load-bearing rather than tidy (approval-half-test.sh's header says
# why each of these matters; the same variables are pinned here):
#   $HOME, $CLAUDE_CONFIG_DIR  a throwaway home -- the real user store and
#                             settings are never read
#   $QONTINUI_ROOT             the sandbox root, so L2's sibling sweep probes
#                             nothing outside it; it carries ONE symlink,
#                             qontinui-claude-config/scripts -> this repo's
#                             scripts/, so a staged lone copy of the subject
#                             still resolves its helpers (resolver rung 3)
#   $QONTINUI_RUNNER_URL       the stub, or a dead port; it OVERRIDES the 9876
#                             default for every runner read, including the
#                             frontend-state /health read the claim stamps
#                             runner_build= from (that override is what this
#                             suite's runner_build assertion found missing)
#   $COORD_HTTP_URL            a dead port for L3/L4/L5's bearer doors
#   $COORD_REVIVE_NO_MINT      set -- the in-process mint is a POST that on a
#                             real runner EVICTS a live peer's slot
#   $COORD_REVIVE_LOADAVG_FILE a fixture file; `pgrep`, `uptime` and `nproc`
#                             are shadowed on $PATH by stubs the case controls
#
# Hermetic: no real door, no real runner, no network beyond 127.0.0.1, nothing
# written outside the sandbox.

set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
SCRIPT="${MC_SUBJECT:-$HERE/coord-revive.sh}"
REPO_ROOT="$(cd "$HERE/../../.." && pwd)"
PASS=0
FAIL=0
FAILED_CASES=()

[ -r "$SCRIPT" ] || { echo "FATAL: $SCRIPT not readable"; exit 1; }
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
cleanup() { [ -n "$STUB_PID" ] && kill "$STUB_PID" 2>/dev/null; rm -rf "$SANDBOX"; }
trap cleanup EXIT

FAKE_HOME="$SANDBOX/home"; mkdir -p "$FAKE_HOME/.qontinui"
ROOT="$SANDBOX/root"; mkdir -p "$ROOT/qontinui-claude-config"
ln -s "$REPO_ROOT/scripts" "$ROOT/qontinui-claude-config/scripts"
BIN="$SANDBOX/bin"; mkdir -p "$BIN"
LOADAVG="$SANDBOX/loadavg"
DEAD="http://127.0.0.1:1"

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
            self._send(200, {"jsonrpc": "2.0", "id": rid, "result": tool_text({"device_id": "stub"})})
            return
        self._send(200, {"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "no such method"}})


srv = HTTPServer(("127.0.0.1", 0), H)
print(srv.server_address[1], flush=True)
srv.serve_forever()
PYEOF
"$PY" "$SANDBOX/stub.py" "$STUB_MODE" > "$SANDBOX/port" 2>"$SANDBOX/stub.err" &
STUB_PID=$!
PORT=""
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  PORT="$(tr -d '[:space:]' < "$SANDBOX/port" 2>/dev/null)"
  [ -n "$PORT" ] && break
  sleep 0.2
done
[ -n "$PORT" ] || { echo "FATAL: the stub door never reported a port"; cat "$SANDBOX/stub.err"; exit 1; }
STUB="http://127.0.0.1:$PORT"

# ---------------------------------------------------------------- the runner
# run_case <name> <runner-url> [args...] -> RC, OUT, ERR, CLAIM (the claim line)
CASE_N=0
run_case() {
  local name="$1" runner="$2"; shift 2
  CASE_N=$((CASE_N + 1))
  local cwd="$SANDBOX/case-$CASE_N"; mkdir -p "$cwd"
  CASE_CWD="$cwd"
  if [ "${WITH_DOOR:-0}" = "1" ]; then
    printf '{"mcpServers":{"coord-mcp":{"url":"%s/coord-mcp","headers":{"X-Coord-Mcp-Proxy-Key":"stub-nonce"}}}}\n' "$STUB" > "$cwd/.mcp.json"
  fi
  OUT="$(cd "$cwd" && PATH="$BIN:$PATH" HOME="$FAKE_HOME" USERPROFILE="$FAKE_HOME" CLAUDE_CONFIG_DIR="$FAKE_HOME/cc" \
        QONTINUI_ROOT="$ROOT" QONTINUI_RUNNER_URL="$runner" COORD_HTTP_URL="$DEAD" \
        COORD_REVIVE_NO_MINT=1 COORD_REVIVE_LOADAVG_FILE="$LOADAVG" \
        COORD_REVIVE_HEALTH_SLOW_MS="${SLOW_MS:-5000}" \
        bash "$SCRIPT" "$@" 2>"$SANDBOX/err")"
  RC=$?
  ERR="$(cat "$SANDBOX/err")"
  CLAIM="$(printf '%s\n' "$OUT" | grep -m1 '^FLOOR-CLAIM: ' || true)"
  CLAIMS_SEEN="${CLAIMS_SEEN:-}
$CLAIM"
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
COORD_REVIVE_NO_BOOTSTRAP=1 run_case u "$DEAD" --floor-claim
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
OUT="$(cd "$CALL_CWD" && HOME="$FAKE_HOME" QONTINUI_ROOT="$ROOT" bash "$SCRIPT" call coord_memory_search '{"query":"x"}' 2>"$SANDBOX/err")"
RC=$?; ERR="$(cat "$SANDBOX/err")"
assert_eq  "(i) call exits 0 (the tool answered)"   "0" "$RC"
assert_has "(i) stdout is still the raw result"      'records' "$OUT"
assert_has "(i) stderr carries the envelope advisory" "0 rows under known list keys" "$ERR"
assert_has "(i) ...and names the UNKNOWN(envelope) arm" "UNKNOWN(envelope) until confirmed" "$ERR"
echo live > "$STUB_MODE"
OUT="$(cd "$CALL_CWD" && HOME="$FAKE_HOME" QONTINUI_ROOT="$ROOT" bash "$SCRIPT" call coord_memory_search '{"query":"x"}' 2>"$SANDBOX/err")"
ERR="$(cat "$SANDBOX/err")"
assert_lacks "(i) no advisory when rows are present"  "0 rows under known list keys" "$ERR"

# ================================================================ the discharge
if [ "${MC_MUTANT:-0}" = "1" ]; then
  :   # a re-run must not drive its own mutations
else
  MC_LIB="$REPO_ROOT/scripts/lib/mutation-control.sh"
  if [ -r "$MC_LIB" ]; then
    echo "== discharge: the assertions above are shown to be able to fail"
    # shellcheck source=/dev/null
    . "$MC_LIB"
    mc_ok()  { ok "$*"; }
    mc_bad() { bad "$*"; }
    mc_init ".claude/skills/coord-revive/floor-claim-test.sh" "$SANDBOX"
    # The staging control: an UNMUTATED lone copy in the mutant's layout must
    # run GREEN, or every red below would be attributing the staging.
    mkdir -p "$SANDBOX/ctl"
    cp "$SCRIPT" "$SANDBOX/ctl/coord-revive.sh"
    if MC_MUTANT=1 MC_SUBJECT="$SANDBOX/ctl/coord-revive.sh" bash "$0" >"$SANDBOX/ctl.log" 2>&1; then
      ok "staging control: an UNMUTATED lone copy runs GREEN"
    else
      bad "staging control: an UNMUTATED lone copy is already RED -- see $SANDBOX/ctl.log"
      tail -20 "$SANDBOX/ctl.log"
    fi
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
    mc_trailer "$((PASS + FAIL))"
  else
    echo "SKIP  discharge: no readable $MC_LIB"
  fi
fi

echo "--------------------------------------------------------------"
if [ "$FAIL" -eq 0 ]; then
  echo "PASS  $PASS assertions, 0 failures"
  exit 0
fi
echo "$FAIL of $((PASS + FAIL)) assertions FAILED:"
for f in "${FAILED_CASES[@]}"; do echo "  - $f"; done
exit 1
