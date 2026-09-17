#!/usr/bin/env bash
# Self-test for /coord-revive's SESSION TENANT on its two credential mints -- plan
# 2026-09-10-spawn-tenant-never-reaches-the-session-coord-credential, P3 (the
# runner's get_coord_device_token) and P5a (coord's POST /agents/credential).
#
# WHAT IT ASSERTS. Every case runs the real cascade against ONE stub that plays
# both the runner (GET /health, GET /control/sessions/info, POST
# /ui-bridge/invoke/*) and coord (POST /mcp, POST /agents/credential), and reads
# back the request bodies the stub recorded:
#
#   (t1) $QONTINUI_TENANT_ID=B, a P3 runner      -> invoke body {"tenantId":B}, L4 LIVE
#   (t2) no env; the census row for $QONTINUI_TERMINAL_ID says B
#                                                -> tenantId B, source named "tenancy.row"
#   (t3) the census row names no tenant but the credential is resolved to C
#                                                -> tenantId C, source "tenancy.credential"
#   (t4) the census has NO tenancy block (a build predating it), P3 runner
#                                                -> body {}, the 409 surfaces as
#                                                   RUNNER_MINT_TENANT_REQUIRED naming
#                                                   tenancy_block_absent, never
#                                                   RUNNER_SIGNED_OUT, and the
#                                                   default-slot websocket door is NOT asked
#   (t5) a runner that IGNORES tenantId answers A's token when B was asked
#                                                -> RUNNER_MINT_WRONG_TENANT, L4 not LIVE
#   (t6) no tenant known on a single-slot runner -> body {}, L4 LIVE (today's call)
#   (t7) L5 with $QONTINUI_TENANT_ID=B           -> credential body carries tenant_id B
#   (t8) L5 with no tenant, coord in `live` mode -> 422 tenant_ambiguous surfaces as
#                                                   BOOTSTRAP_TENANT_AMBIGUOUS naming the remedy
#
# L4 SOURCE 3 (the in-process nonce mint, POST /coord-mcp/provision-session, P2):
#   (n1) tenant B, a P2 runner pins the nonce    -> provision body carries tenant_id B,
#                                                   coord_query_identity over it names B,
#                                                   VERDICT LIVE transport=loopback-proxy-minted
#   (n2) tenant B, a runner that ignores the field mints for A
#                                                -> NONCE_MINT_WRONG_TENANT, not LIVE
#   (n3) tenant B, the identity read-back fails  -> NONCE_MINT_UNVERIFIED_TENANT, not LIVE
#   (n4) 422 COORD_MCP_PROVISION_TENANT_NOT_PAIRED -> NONCE_MINT_TENANT_NOT_PAIRED
#   (n5) 400 COORD_MCP_PROVISION_INVALID_TENANT    -> NONCE_MINT_TENANT_INVALID
#   (n6) 503 COORD_MCP_PROVISION_TENANT_UNKNOWN    -> NONCE_MINT_TENANT_UNKNOWN
#   (n7) no tenant known                         -> body {cwd} only, LIVE, and the LIVE
#                                                   names the acting tenant it read
#
# REVIEW ROUND (W1-W4, S2):
#   (w1) a named tenant answered 2xx null        -> RUNNER_MINT_TENANT_NOT_PAIRED
#   (w2) L5 400 tenant_not_bound                 -> BOOTSTRAP_TENANT_NOT_BOUND (stale var named)
#   (w3) $QONTINUI_RUNNER_URL unset              -> the census and the mint go to the port in
#                                                   $QONTINUI_RUNNER_API_PORT (a non-9876 stub)
#   (w4) every LIVE names its tenant; L5 checks the minted token's claim
#        (legacy coord -> BOOTSTRAP_WRONG_TENANT, claimless -> BOOTSTRAP_UNVERIFIED_TENANT)
#   (c1) TWO runners: the spawning runner A ($QONTINUI_RUNNER_API_PORT) lists the
#        terminal with tenant B but cannot mint; a SIBLING runner B (named by a
#        sibling .mcp.json) 404s the census and ignores tenant arguments
#                                                -> the session tenant stays B (never
#                                                   re-read from the sibling), both mints
#                                                   are WRONG_TENANT, never LIVE
#   (s2) TERMINAL_ID unset, census non-200, census status != ok, malformed
#        $QONTINUI_TENANT_ID, row outranks credential, RUNNER_MINT_TENANT_INVALID,
#        and base64url `-`/`_` + every padding length in the claim decode
#
# THE DISCHARGE (last section): staged copies of coord-revive.sh with the tenant
# dropped from the invoke body, the wrong-tenant check deleted, the L5 tenant_id
# dropped, the nonce mint's tenant dropped, its identity verification skipped,
# and its tenant refusals collapsed must each redden a re-run.
#
# ISOLATION: $HOME and $CLAUDE_CONFIG_DIR are a throwaway home; $QONTINUI_ROOT is
# the sandbox (one symlink to this repo's scripts/); $QONTINUI_RUNNER_URL,
# $COORD_HTTP_URL and $QONTINUI_WEB_HTTP_URL point at the stub or a dead port;
# $COORD_REVIVE_NO_MINT is set. No real door, no network beyond 127.0.0.1, no
# token printed -- the stub's tokens are unsigned fixtures.

set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
SCRIPT="${MC_SUBJECT:-$HERE/coord-revive.sh}"
REPO_ROOT="$(cd "$HERE/../../.." && pwd)"
PASS=0
FAIL=0
FAILED_CASES=()

[ -r "$SCRIPT" ] || { echo "FATAL: $SCRIPT not readable"; exit 1; }
PY="$(command -v python3 || command -v python || true)"
[ -n "$PY" ] || { echo "FATAL: no python3/python for the stub"; exit 1; }

ok()  { PASS=$((PASS + 1)); echo "ok    $1"; }
bad() { FAIL=$((FAIL + 1)); FAILED_CASES+=("$1"); echo "FAIL  $1"; }
assert_eq() { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (want '$2', got '$3')"; fi; }
assert_has() { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1 (missing '$2')" ;; esac; }
assert_lacks() { case "$3" in *"$2"*) bad "$1 (found '$2')" ;; *) ok "$1" ;; esac; }

SANDBOX="$(mktemp -d)" || { echo "FATAL: mktemp -d failed"; exit 1; }
STUB_PID=""; STUB_B_PID=""
cleanup() {
  [ -n "$STUB_PID" ] && kill "$STUB_PID" 2>/dev/null
  [ -n "$STUB_B_PID" ] && kill "$STUB_B_PID" 2>/dev/null
  rm -rf "$SANDBOX"
}
trap cleanup EXIT

FAKE_HOME="$SANDBOX/home"; mkdir -p "$FAKE_HOME/.qontinui"
printf 'stub-loopback-key\n' > "$FAKE_HOME/.qontinui/runner-loopback-key"
printf '{"device_id":"11111111-1111-4111-8111-111111111111"}\n' > "$FAKE_HOME/.qontinui/machine.json"
ROOT="$SANDBOX/root"; mkdir -p "$ROOT/qontinui-claude-config"
ln -s "$REPO_ROOT/scripts" "$ROOT/qontinui-claude-config/scripts"
DEAD="http://127.0.0.1:1"
TA="aaaaaaaa-0000-4000-8000-00000000000a"
TB="bbbbbbbb-0000-4000-8000-00000000000b"
TC="cccccccc-0000-4000-8000-00000000000c"

MODE_DIR="$SANDBOX/mode"; mkdir -p "$MODE_DIR"
setmode() { printf '%s' "$2" > "$MODE_DIR/$1"; }
cat > "$SANDBOX/stub.py" <<'PYEOF'
import base64, json, os, sys, time
from http.server import BaseHTTPRequestHandler, HTTPServer

MODE_DIR, LOG = sys.argv[1], sys.argv[2]


def mode(name, default=""):
    try:
        with open(os.path.join(MODE_DIR, name), encoding="utf-8") as fh:
            return fh.read().strip()
    except OSError:
        return default


def b64(obj):
    return base64.urlsafe_b64encode(json.dumps(obj).encode()).decode().rstrip("=")


def token(tenant):
    exp = int(time.time()) + 3600
    payload = {"sub_type": "device", "exp": exp}
    if tenant:
        payload["tenant_id"] = tenant
    # `claim_pad`: a filler that puts base64url `-` and `_` into the payload
    # segment and moves its length through every value mod 4.
    pad = mode("claim_pad", "")
    if pad != "":
        payload["f"] = "??~~" * 3 + "x" * int(pad)
    seg = b64(payload)
    with open(os.path.join(MODE_DIR, "last_payload_segment"), "w", encoding="utf-8") as fh:
        fh.write(seg)
    return "%s.%s.sig" % (b64({"alg": "none"}), seg)


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

    def _log(self, raw):
        with open(LOG, "a", encoding="utf-8") as fh:
            fh.write("%s %s %s\n" % (self.command, self.path, raw.decode("utf-8", "replace")))

    def do_GET(self):
        self._log(b"")
        if self.path.startswith("/health"):
            self._send(200, {"frontendReady": True, "frontendState": "ready", "buildId": "stub-build"})
            return
        if self.path.startswith("/control/sessions/info"):
            census = mode("census", "none")
            row = {"identity": {"terminalId": "term-1", "claudeSessionId": "s"}, "available": True}
            if census == "row":
                row["tenancy"] = {"row": {"tenantId": mode("row_tenant")}, "credential": {"status": "unknown", "tenantId": None, "reason": "no_session_nonce"}}
            elif census == "credential":
                row["tenancy"] = {"row": {"tenantId": None}, "credential": {"status": "resolved", "tenantId": mode("cred_tenant"), "slot": "tenant"}}
            elif census == "both":
                row["tenancy"] = {"row": {"tenantId": mode("row_tenant")}, "credential": {"status": "resolved", "tenantId": mode("cred_tenant"), "slot": "tenant"}}
            elif census == "no-tenancy":
                pass
            elif census == "down":
                self._send(503, {"error": "census down"})
                return
            elif census == "unavailable":
                self._send(200, {"success": True, "data": {"status": "unavailable", "reason": "lifecycle_store_unavailable", "sessions": []}})
                return
            else:
                self._send(404, {"error": "not found"})
                return
            other = {"identity": {"terminalId": "term-other"}, "tenancy": {"row": {"tenantId": "dddddddd-0000-4000-8000-00000000000d"}}}
            self._send(200, {"success": True, "data": {"status": "ok", "reason": None, "sessions": [other, row]}})
            return
        if self.path.startswith("/coord/agent-findings"):
            self._send(200, {"findings": [], "count": 0})
            return
        self._send(404, {"error": "not found"})

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n) if n else b""
        self._log(raw)
        try:
            req = json.loads(raw.decode("utf-8")) if raw else {}
        except Exception:
            req = {}
        if self.path == "/coord-mcp/provision-session":
            if self.headers.get("X-Qontinui-Loopback-Key") != "stub-loopback-key":
                self._send(403, {"success": False, "code": "COORD_MCP_PROVISION_NO_HANDSHAKE", "error": "no handshake"})
                return
            nm = mode("nonce_runner", "absent")
            port = self.server.server_address[1]
            refusals = {
                "refuse-invalid": (400, "COORD_MCP_PROVISION_INVALID_TENANT"),
                "refuse-not-paired": (422, "COORD_MCP_PROVISION_TENANT_NOT_PAIRED"),
                "refuse-unknown": (503, "COORD_MCP_PROVISION_TENANT_UNKNOWN"),
            }
            if nm == "absent":
                self._send(404, {"error": "not found"})
                return
            if nm in refusals:
                code, c = refusals[nm]
                self._send(code, {"success": False, "code": c, "error": "stub refusal"})
                return
            tenant = req.get("tenant_id") if nm == "p2" else None
            nonce = "n-" + (tenant or mode("default_tenant"))
            url = "http://127.0.0.1:%d/coord-mcp" % port
            self._send(200, {"mcpServers": {"coord-mcp": {"type": "http", "url": url,
                "headers": {"Authorization": "Bearer " + nonce, "X-Coord-Mcp-Proxy-Key": nonce}}}})
            return
        if self.path == "/coord-mcp":
            key = self.headers.get("X-Coord-Mcp-Proxy-Key") or ""
            if not key.startswith("n-"):
                self._send(401, {"code": "COORD_MCP_PROXY_UNAUTHORIZED"})
                return
            rid = req.get("id", 1)
            if req.get("method") == "tools/list":
                self._send(200, {"jsonrpc": "2.0", "id": rid, "result": {"tools": [{"name": "coord_query_identity"}, {"name": "coord_memory_search"}]}})
                return
            name = (req.get("params") or {}).get("name")
            if name == "coord_query_identity":
                if mode("identity", "ok") == "no-tenant":
                    text = json.dumps({"principal_kind": "device", "tenant_slug": "x"})
                    self._send(200, {"jsonrpc": "2.0", "id": rid, "result": {"content": [{"type": "text", "text": text}]}})
                    return
                text = json.dumps({"principal_kind": "device", "tenant_id": key[2:]})
                self._send(200, {"jsonrpc": "2.0", "id": rid, "result": {"content": [{"type": "text", "text": text}]}})
                return
            text = json.dumps({"hits": [{"id": "m1", "title": "x"}], "count": 1})
            self._send(200, {"jsonrpc": "2.0", "id": rid, "result": {"content": [{"type": "text", "text": text}]}})
            return
        if self.path == "/ui-bridge/invoke/get_coord_device_token":
            asked = req.get("tenantId")
            runner = mode("runner", "p3")
            if runner == "absent":
                self._send(404, {"error": "not found"})
            elif runner == "old":
                self._send(200, {"success": True, "data": token(mode("default_tenant"))})
            elif runner == "p3-null-named":
                self._send(200, {"success": True, "data": None})
            elif runner == "p3-invalid":
                self._send(400, {"success": False, "error": "invoke proxy: invalid args for in-process command 'get_coord_device_token': get_coord_device_token:tenant_invalid: tenant_id is not a tenant uuid"})
            elif runner == "p3-multi":
                if asked:
                    self._send(200, {"success": True, "data": token(asked)})
                else:
                    self._send(409, {"success": False, "error": "invoke proxy: in-process invoke of 'get_coord_device_token' refused: get_coord_device_token:tenant_required: this runner holds coord credentials for 2 tenants"})
            else:  # p3-single
                self._send(200, {"success": True, "data": token(asked or mode("default_tenant"))})
            return
        if self.path == "/ui-bridge/invoke/get_access_token_for_websocket":
            if mode("websocket", "live") == "absent":
                self._send(404, {"error": "not found"})
                return
            self._send(200, {"success": True, "data": token(mode("default_tenant"))})
            return
        if self.path == "/mcp":
            rid = req.get("id", 1)
            self._send(200, {"jsonrpc": "2.0", "id": rid, "result": {"tools": [{"name": "coord_query_identity"}, {"name": "coord_memory_search"}]}})
            return
        if self.path == "/agents/credential":
            cm = mode("coord", "live")
            if cm == "not-bound" and req.get("tenant_id"):
                self._send(400, {"error": "tenant_not_bound"})
                return
            if cm == "legacy":
                self._send(200, {"token": token(mode("default_tenant")), "token_exp": 0})
                return
            if cm == "claimless":
                self._send(200, {"token": token(None), "token_exp": 0})
                return
            if not req.get("tenant_id") and cm == "live":
                self._send(422, {"error": "tenant_ambiguous", "code": "tenant_ambiguous", "message": "device bound to 2 tenants; send tenant_id"})
                return
            self._send(200, {"token": token(req.get("tenant_id") or mode("default_tenant")), "token_exp": 0})
            return
        self._send(404, {"error": "not found"})


srv = HTTPServer(("127.0.0.1", 0), H)
print(srv.server_address[1], flush=True)
srv.serve_forever()
PYEOF
REQLOG="$SANDBOX/requests.log"; : > "$REQLOG"
"$PY" "$SANDBOX/stub.py" "$MODE_DIR" "$REQLOG" > "$SANDBOX/port" 2>"$SANDBOX/stub.err" &
STUB_PID=$!
PORT=""
for _ in $(seq 1 25); do
  PORT="$(tr -d '[:space:]' < "$SANDBOX/port" 2>/dev/null)"
  [ -n "$PORT" ] && break
  sleep 0.2
done
[ -n "$PORT" ] || { echo "FATAL: the stub never reported a port"; cat "$SANDBOX/stub.err"; exit 1; }
STUB="http://127.0.0.1:$PORT"

# run_case <runner-url> <coord-url> [env assignments...] -> RC, OUT, ERR; REQLOG reset per case
CASE_N=0
run_case() {
  local runner="$1" coord="$2"; shift 2
  CASE_N=$((CASE_N + 1))
  local cwd="$SANDBOX/case-$CASE_N"; mkdir -p "$cwd"
  : > "$REQLOG"
  # `-` as the runner leaves $QONTINUI_RUNNER_URL UNSET (the W3 case passes the
  # stub through $QONTINUI_RUNNER_API_PORT instead). Every other case pins it,
  # and the session's own runner variables are always cleared, so no case can
  # reach a real runner on this box.
  local runner_env="QONTINUI_RUNNER_URL=$runner"
  [ "$runner" = "-" ] && runner_env="COORD_REVIVE_TEST_NO_RUNNER_URL=1"
  OUT="$(cd "$cwd" && env -u QONTINUI_TENANT_ID -u QONTINUI_TERMINAL_ID -u COORD_DEVICE_JWT -u COORD_AGENT_JWT \
        -u QONTINUI_RUNNER_URL -u QONTINUI_RUNNER_API_PORT -u QONTINUI_RUNNER_PORT \
        HOME="$FAKE_HOME" USERPROFILE="$FAKE_HOME" CLAUDE_CONFIG_DIR="$FAKE_HOME/cc" \
        QONTINUI_ROOT="$ROOT" "$runner_env" COORD_HTTP_URL="$coord" \
        QONTINUI_WEB_HTTP_URL="$DEAD" QONTINUI_MACHINE_ID= \
        COORD_REVIVE_NO_MINT=1 COORD_REVIVE_PROBE_TIMEOUT=5 COORD_REVIVE_MINT_TIMEOUT=10 \
        "$@" bash "$SCRIPT" 2>"$SANDBOX/err")"
  RC=$?
  ERR="$(cat "$SANDBOX/err")"
  REQS="$(cat "$REQLOG")"
}
identity_calls() { printf '%s\n' "$REQS" | grep -c 'coord_query_identity'; }
provision_body() { printf '%s\n' "$REQS" | sed -n 's#^POST /coord-mcp/provision-session ##p' | head -n 1; }
invoke_body() { printf '%s\n' "$REQS" | sed -n 's#^POST /ui-bridge/invoke/get_coord_device_token ##p' | head -n 1; }
cred_body() { printf '%s\n' "$REQS" | sed -n 's#^POST /agents/credential ##p' | head -n 1; }

echo "== (t1) \$QONTINUI_TENANT_ID names the tenant -> sent to a P3 runner, L4 LIVE"
setmode runner p3-multi; setmode census none; setmode default_tenant "$TA"
run_case "$STUB" "$STUB" QONTINUI_TENANT_ID="$TB"
assert_eq  "(t1) the invoke body names tenant B" "{\"tenantId\":\"$TB\"}" "$(invoke_body)"
assert_has "(t1) VERDICT: LIVE" "VERDICT: LIVE" "$OUT"
assert_has "(t1) the PARTIAL block states the tenant established" "PARTIAL: tenant established: the minted token's tenant_id claim is $TB; sent tenant $TB (from \$QONTINUI_TENANT_ID)" "$OUT"
assert_lacks "(t1) no token on stdout" "sig" "$(printf '%s' "$OUT" | grep -o 'eyJ[A-Za-z0-9_-]*\.[A-Za-z0-9_-]*\.sig' || true)"

echo "== (t2) the runner's session census row names the tenant"
setmode census row; setmode row_tenant "$TB"
run_case "$STUB" "$STUB" QONTINUI_TERMINAL_ID=term-1
assert_eq  "(t2) the invoke body names the row tenant" "{\"tenantId\":\"$TB\"}" "$(invoke_body)"
assert_has "(t2) VERDICT: LIVE" "VERDICT: LIVE" "$OUT"
assert_has "(t2) the census was asked" "GET /control/sessions/info" "$REQS"

echo "== (t3) the row names none; the resolved credential tenant is used"
setmode census credential; setmode cred_tenant "$TC"
run_case "$STUB" "$STUB" QONTINUI_TERMINAL_ID=term-1
assert_eq  "(t3) the invoke body names the credential tenant" "{\"tenantId\":\"$TC\"}" "$(invoke_body)"

echo "== (t4) a census with no tenancy block, on a multi-slot P3 runner -> typed refusal"
setmode census no-tenancy
run_case "$STUB" "$DEAD" QONTINUI_TERMINAL_ID=term-1 COORD_REVIVE_NO_BOOTSTRAP=1
assert_eq  "(t4) no tenant is invented" "{}" "$(invoke_body)"
assert_has "(t4) RUNNER_MINT_TENANT_REQUIRED" "RUNNER_MINT_TENANT_REQUIRED" "$ERR"
assert_has "(t4) the reason names the absent block" "tenancy_block_absent" "$ERR"
assert_has "(t4) the runner's own text is surfaced verbatim" "get_coord_device_token:tenant_required" "$ERR"
assert_lacks "(t4) never read as signed out" "RUNNER_SIGNED_OUT" "$ERR"
assert_lacks "(t4) the default-slot websocket door is NOT asked" "get_access_token_for_websocket" "$REQS"
assert_lacks "(t4) not LIVE" "VERDICT: LIVE" "$OUT"

echo "== (t5) a runner that ignores tenantId answers the default slot -> refused, not LIVE"
setmode runner old; setmode default_tenant "$TA"; setmode census none
run_case "$STUB" "$STUB" QONTINUI_TENANT_ID="$TB" COORD_REVIVE_NO_BOOTSTRAP=1
assert_has "(t5) RUNNER_MINT_WRONG_TENANT" "RUNNER_MINT_WRONG_TENANT" "$ERR"
assert_has "(t5) names the claim that came back" "claims tenant $TA" "$ERR"
assert_lacks "(t5) the wrong tenant's token is not probed against coord" "POST /mcp" "$REQS"
assert_lacks "(t5) not LIVE" "VERDICT: LIVE" "$OUT"

echo "== (t6) no tenant known, single-slot runner -> today's call, LIVE"
setmode runner p3-single; setmode default_tenant "$TA"
run_case "$STUB" "$STUB"
assert_eq  "(t6) the invoke body is empty" "{}" "$(invoke_body)"
assert_has "(t6) VERDICT: LIVE" "VERDICT: LIVE" "$OUT"

echo "== (t7) L5 carries the session tenant"
setmode coord live
run_case "$DEAD" "$STUB" QONTINUI_TENANT_ID="$TB"
assert_has "(t7) the credential body names tenant_id B" "\"tenant_id\":\"$TB\"" "$(cred_body)"
assert_has "(t7) the device_id is still sent" "\"device_id\":\"11111111-1111-4111-8111-111111111111\"" "$(cred_body)"
assert_has "(t7) L5 LIVE" "transport=https-bootstrap-agent-jwt" "$OUT"
assert_has "(t7) the LIVE names the token's tenant claim" "TENANT: token tenant_id claim $TB; sent tenant $TB (from" "$OUT"

echo "== (t8) L5 with no tenant on a multi-bound device -> BOOTSTRAP_TENANT_AMBIGUOUS"
run_case "$DEAD" "$STUB"
assert_lacks "(t8) no tenant_id invented" "tenant_id" "$(cred_body)"
assert_has "(t8) BOOTSTRAP_TENANT_AMBIGUOUS" "BOOTSTRAP_TENANT_AMBIGUOUS" "$ERR"
assert_has "(t8) the remedy names the variable" "QONTINUI_TENANT_ID" "$ERR"
assert_lacks "(t8) not the generic device-rejected verdict" "BOOTSTRAP_DEVICE_REJECTED" "$ERR"

# ================================================================ L4 source 3
# The nonce mint runs only with the mint enabled (COORD_REVIVE_NO_MINT= empty) and
# BEFORE the invoke door, so the invoke door is made absent (404) and L5 is off:
# a LIVE here can only be the nonce's.
setmode runner absent; setmode census none; setmode default_tenant "$TA"; setmode identity ok

echo "== (n1) tenant B, a P2 runner pins the nonce -> verified, LIVE"
setmode nonce_runner p2
run_case "$STUB" "$DEAD" QONTINUI_TENANT_ID="$TB" COORD_REVIVE_NO_MINT= COORD_REVIVE_NO_BOOTSTRAP=1
assert_has "(n1) the provision body names tenant_id B" "\"tenant_id\": \"$TB\"" "$(provision_body | sed 's/":"/": "/g')"
assert_eq "(n1) the acting tenant was read back (probe e2e + verification)" "2" "$(identity_calls)"
assert_has "(n1) VERDICT LIVE over the minted nonce" "transport=loopback-proxy-minted" "$OUT"
assert_has "(n1) the LIVE names the verified acting tenant" "TENANT: acting tenant $TB, verified" "$OUT"
assert_has "(n1) and says what was sent" "sent tenant $TB (from \$QONTINUI_TENANT_ID)" "$OUT"

echo "== (n2) tenant B, a runner that ignores tenant_id mints for A -> WRONG_TENANT, not LIVE"
setmode nonce_runner old
run_case "$STUB" "$DEAD" QONTINUI_TENANT_ID="$TB" COORD_REVIVE_NO_MINT= COORD_REVIVE_NO_BOOTSTRAP=1
assert_has "(n2) NONCE_MINT_WRONG_TENANT" "NONCE_MINT_WRONG_TENANT" "$ERR"
assert_has "(n2) names the tenant coord reported" "names tenant $TA" "$ERR"
assert_lacks "(n2) not LIVE" "VERDICT: LIVE" "$OUT"

echo "== (n3) tenant B, the identity answer names no tenant -> UNVERIFIED_TENANT, not LIVE"
setmode nonce_runner p2; setmode identity no-tenant
run_case "$STUB" "$DEAD" QONTINUI_TENANT_ID="$TB" COORD_REVIVE_NO_MINT= COORD_REVIVE_NO_BOOTSTRAP=1
setmode identity ok
assert_has "(n3) NONCE_MINT_UNVERIFIED_TENANT" "NONCE_MINT_UNVERIFIED_TENANT" "$ERR"
assert_has "(n3) names why it could not be read" "carried no string tenant_id" "$ERR"
assert_lacks "(n3) not LIVE" "VERDICT: LIVE" "$OUT"

for spec in "n4 refuse-not-paired NONCE_MINT_TENANT_NOT_PAIRED device pair --tenant-id" \
            "n5 refuse-invalid NONCE_MINT_TENANT_INVALID not a uuid" \
            "n6 refuse-unknown NONCE_MINT_TENANT_UNKNOWN NOT 'unpaired'"; do
  set -- $spec
  cid="$1"; rmode="$2"; verdict="$3"; shift 3; hint="$*"
  echo "== ($cid) $rmode -> $verdict"
  setmode nonce_runner "$rmode"
  run_case "$STUB" "$DEAD" QONTINUI_TENANT_ID="$TB" COORD_REVIVE_NO_MINT= COORD_REVIVE_NO_BOOTSTRAP=1
  assert_has "($cid) $verdict" "$verdict" "$ERR"
  assert_has "($cid) names its remedy" "$hint" "$ERR"
  assert_lacks "($cid) never MINT_REFUSED (the opt-in/handshake verdict)" "MINT_REFUSED (" "$ERR"
  assert_lacks "($cid) not LIVE" "VERDICT: LIVE" "$OUT"
done

echo "== (n7) no tenant known -> {cwd} only, no identity read, LIVE as before"
setmode nonce_runner p2
run_case "$STUB" "$DEAD" COORD_REVIVE_NO_MINT= COORD_REVIVE_NO_BOOTSTRAP=1
assert_lacks "(n7) no tenant_id sent" "tenant_id" "$(provision_body)"
assert_eq "(n7) the probe's end-to-end identity call plus the tenant read-back" "2" "$(identity_calls)"
assert_has "(n7) the LIVE still names the acting tenant it read" "TENANT: acting tenant $TA (coord_query_identity" "$OUT"
assert_has "(n7) VERDICT LIVE over the minted nonce" "transport=loopback-proxy-minted" "$OUT"
setmode nonce_runner absent

# ================================================================ review round
setmode nonce_runner absent; setmode census none; setmode default_tenant "$TA"; setmode coord live

echo "== (w1) a named tenant answered with null -> RUNNER_MINT_TENANT_NOT_PAIRED"
setmode runner p3-null-named
run_case "$STUB" "$DEAD" QONTINUI_TENANT_ID="$TB" COORD_REVIVE_NO_BOOTSTRAP=1
assert_has   "(w1) RUNNER_MINT_TENANT_NOT_PAIRED" "RUNNER_MINT_TENANT_NOT_PAIRED" "$ERR"
assert_has   "(w1) names the tenant asked for" "tenant $TB from" "$ERR"
assert_lacks "(w1) never RUNNER_SIGNED_OUT" "RUNNER_SIGNED_OUT" "$ERR"

echo "== (w1b) a tenant-less null is still RUNNER_SIGNED_OUT"
run_case "$STUB" "$DEAD" COORD_REVIVE_NO_BOOTSTRAP=1
assert_has   "(w1b) RUNNER_SIGNED_OUT" "RUNNER_SIGNED_OUT" "$ERR"

echo "== (w2) L5 400 tenant_not_bound -> BOOTSTRAP_TENANT_NOT_BOUND"
setmode coord not-bound
run_case "$DEAD" "$STUB" QONTINUI_TENANT_ID="$TB"
assert_has   "(w2) BOOTSTRAP_TENANT_NOT_BOUND" "BOOTSTRAP_TENANT_NOT_BOUND" "$ERR"
assert_has   "(w2) names a stale variable as the likely cause" "STALE \$QONTINUI_TENANT_ID" "$ERR"
assert_lacks "(w2) not the generic device-rejected verdict" "BOOTSTRAP_DEVICE_REJECTED" "$ERR"

echo "== (w4) L5 against a coord that ignores tenant_id -> BOOTSTRAP_WRONG_TENANT"
setmode coord legacy
run_case "$DEAD" "$STUB" QONTINUI_TENANT_ID="$TB"
assert_has   "(w4) BOOTSTRAP_WRONG_TENANT" "BOOTSTRAP_WRONG_TENANT" "$ERR"
assert_has   "(w4) names the claim" "claims tenant $TA" "$ERR"
assert_lacks "(w4) not LIVE" "VERDICT: LIVE" "$OUT"
assert_lacks "(w4) the wrong token is not used for the control read" "GET /coord/agent-findings" "$REQS"

echo "== (w4b) L5 token with no tenant claim -> BOOTSTRAP_UNVERIFIED_TENANT"
setmode coord claimless
run_case "$DEAD" "$STUB" QONTINUI_TENANT_ID="$TB"
assert_has   "(w4b) BOOTSTRAP_UNVERIFIED_TENANT" "BOOTSTRAP_UNVERIFIED_TENANT" "$ERR"
assert_lacks "(w4b) not LIVE" "VERDICT: LIVE" "$OUT"
setmode coord live

echo "== (w3) \$QONTINUI_RUNNER_URL unset -> the spawning runner's \$QONTINUI_RUNNER_API_PORT"
setmode runner p3-multi; setmode census row; setmode row_tenant "$TB"
case "$PORT" in 9876) bad "(w3) the stub must not be on 9876 for this case to mean anything" ;; esac
run_case - "$STUB" QONTINUI_RUNNER_API_PORT="$PORT" QONTINUI_TERMINAL_ID=term-1
assert_has "(w3) the census on the API_PORT runner was asked" "GET /control/sessions/info" "$REQS"
assert_eq  "(w3) and its row tenant was sent to that runner's mint" "{\"tenantId\":\"$TB\"}" "$(invoke_body)"
assert_has "(w3) the mint that went LIVE was that runner's" "device-jwt@http://127.0.0.1:$PORT source=runner-invoke" "$ERR"
assert_has "(w3) VERDICT: LIVE" "VERDICT: LIVE" "$OUT"

echo "== (s2) \$QONTINUI_TERMINAL_ID unset -> census not asked, the refusal says why"
setmode census row
run_case "$STUB" "$DEAD" COORD_REVIVE_NO_BOOTSTRAP=1
assert_lacks "(s2a) census not asked" "GET /control/sessions/info" "$REQS"
assert_has   "(s2a) reason named" "QONTINUI_TERMINAL_ID is unset" "$ERR"

echo "== (s2) census answers non-200 -> UNKNOWN, reason names the HTTP code"
setmode census down
run_case "$STUB" "$DEAD" QONTINUI_TERMINAL_ID=term-1 COORD_REVIVE_NO_BOOTSTRAP=1
assert_eq  "(s2b) no tenant sent" "{}" "$(invoke_body)"
assert_has "(s2b) reason names the census HTTP code" "HTTP 503" "$ERR"

echo "== (s2) census status != ok -> UNKNOWN with its reason"
setmode census unavailable
run_case "$STUB" "$DEAD" QONTINUI_TERMINAL_ID=term-1 COORD_REVIVE_NO_BOOTSTRAP=1
assert_eq  "(s2c) no tenant sent" "{}" "$(invoke_body)"
assert_has "(s2c) reason names the census's own reason" "census_unavailable:lifecycle_store_unavailable" "$ERR"

echo "== (s2) malformed \$QONTINUI_TENANT_ID -> not sent, and said"
setmode census none
run_case "$STUB" "$DEAD" QONTINUI_TENANT_ID=not-a-uuid COORD_REVIVE_NO_BOOTSTRAP=1
assert_eq  "(s2d) nothing sent" "{}" "$(invoke_body)"
assert_has "(s2d) reason names the variable" "QONTINUI_TENANT_ID is set but is not a uuid" "$ERR"

echo "== (s2) the census row outranks the credential"
setmode census both; setmode row_tenant "$TB"; setmode cred_tenant "$TC"
run_case "$STUB" "$STUB" QONTINUI_TERMINAL_ID=term-1
assert_eq "(s2e) the row tenant is sent" "{\"tenantId\":\"$TB\"}" "$(invoke_body)"

echo "== (s2) the runner answers tenant_invalid -> RUNNER_MINT_TENANT_INVALID"
setmode runner p3-invalid; setmode census none
run_case "$STUB" "$DEAD" QONTINUI_TENANT_ID="$TB" COORD_REVIVE_NO_BOOTSTRAP=1
assert_has   "(s2f) RUNNER_MINT_TENANT_INVALID" "RUNNER_MINT_TENANT_INVALID" "$ERR"
assert_lacks "(s2f) never RUNNER_SIGNED_OUT" "RUNNER_SIGNED_OUT" "$ERR"

echo "== (s2) base64url '-'/'_' and every padding length decode to the right claim"
# GNU base64 decodes unpadded input, so on this box a missing pad step would go
# unseen. A STRICT decoder (the BSD/macOS shape) refuses input whose length is
# not a multiple of 4; the shim below stands in for it, so the padding the
# script restores is actually exercised.
REAL_BASE64="$(command -v base64)"
mkdir -p "$SANDBOX/strictbin"
cat > "$SANDBOX/strictbin/base64" <<EOF
#!/usr/bin/env bash
if [ "\${1:-}" = "-d" ]; then
  in="\$(cat)"
  if [ \$(( \${#in} % 4 )) -ne 0 ]; then echo "base64: invalid input (strict: unpadded)" >&2; exit 1; fi
  printf '%s' "\$in" | "$REAL_BASE64" -d
  exit \$?
fi
exec "$REAL_BASE64" "\$@"
EOF
chmod +x "$SANDBOX/strictbin/base64"
setmode runner p3-multi
SEEN_LENS=""
for n in 0 1 2 3; do
  setmode claim_pad "$n"
  run_case "$STUB" "$STUB" QONTINUI_TENANT_ID="$TB" PATH="$SANDBOX/strictbin:$PATH"
  seg="$(cat "$MODE_DIR/last_payload_segment")"
  SEEN_LENS="$SEEN_LENS $(( ${#seg} % 4 ))"
  case "$seg" in *-*) ;; *) bad "(s2g) pad=$n: the payload segment carries no '-' (fixture did not exercise it)" ;; esac
  case "$seg" in *_*) ;; *) bad "(s2g) pad=$n: the payload segment carries no '_' (fixture did not exercise it)" ;; esac
  assert_lacks "(s2g) pad=$n: no false WRONG_TENANT" "RUNNER_MINT_WRONG_TENANT" "$ERR"
  assert_has   "(s2g) pad=$n: LIVE" "VERDICT: LIVE" "$OUT"
done
rm -f "$MODE_DIR/claim_pad"
for want in 0 2 3; do
  case " $SEEN_LENS " in *" $want "*) ok "(s2g) payload length mod 4 = $want was exercised" ;; *) bad "(s2g) payload length mod 4 = $want never exercised (saw:$SEEN_LENS)" ;; esac
done

# ================================================================ (c1) two runners
echo "== (c1) spawning runner A names tenant B; a sibling runner B ignores it -> never LIVE"
MODE_DIR_B="$SANDBOX/mode-b"; mkdir -p "$MODE_DIR_B"
REQLOG_B="$SANDBOX/requests-b.log"; : > "$REQLOG_B"
"$PY" "$SANDBOX/stub.py" "$MODE_DIR_B" "$REQLOG_B" > "$SANDBOX/port-b" 2>"$SANDBOX/stub-b.err" &
STUB_B_PID=$!
PORT_B=""
for _ in $(seq 1 25); do
  PORT_B="$(tr -d '[:space:]' < "$SANDBOX/port-b" 2>/dev/null)"
  [ -n "$PORT_B" ] && break
  sleep 0.2
done
if [ -z "$PORT_B" ]; then
  bad "(c1) the sibling stub never reported a port"
else
  STUB_B="http://127.0.0.1:$PORT_B"
  # Runner A: the spawning runner. Its census lists term-1 with tenant B; it
  # cannot mint (both mint routes 404, and its websocket door hands back a
  # claimless default-slot token).
  setmode runner absent; setmode nonce_runner absent; setmode census row; setmode row_tenant "$TB"
  rm -f "$MODE_DIR/default_tenant"
  # Runner B: a sibling, reached only through a sibling .mcp.json. It does not
  # list this terminal and IGNORES tenant arguments, minting for tenant A.
  printf 'absent' > "$MODE_DIR_B/census"; printf 'old' > "$MODE_DIR_B/nonce_runner"
  printf 'old' > "$MODE_DIR_B/runner"; printf '%s' "$TA" > "$MODE_DIR_B/default_tenant"
  mkdir -p "$ROOT/sibling-b"
  printf '{"mcpServers":{"coord-mcp":{"type":"http","url":"%s/coord-mcp","headers":{"X-Coord-Mcp-Proxy-Key":"stale"}}}}\n' "$STUB_B" > "$ROOT/sibling-b/.mcp.json"
  : > "$REQLOG_B"
  run_case - "$DEAD" QONTINUI_RUNNER_API_PORT="$PORT" QONTINUI_TERMINAL_ID=term-1 \
    COORD_REVIVE_NO_MINT= COORD_REVIVE_NO_BOOTSTRAP=1
  REQS_B="$(cat "$REQLOG_B")"
  rm -rf "$ROOT/sibling-b"
  assert_has   "(c1) the spawning runner's census was read" "GET /control/sessions/info" "$REQS"
  assert_lacks "(c1) the sibling's census is never read" "GET /control/sessions/info" "$REQS_B"
  assert_has   "(c1) the spawning runner was asked to mint first" "POST /coord-mcp/provision-session" "$REQS"
  assert_has   "(c1) the sibling's nonce is WRONG_TENANT" "NONCE_MINT_WRONG_TENANT" "$ERR"
  assert_has   "(c1) and names the tenant it acts in" "names tenant $TA" "$ERR"
  assert_has   "(c1) the sibling's invoke token is WRONG_TENANT too" "RUNNER_MINT_WRONG_TENANT" "$ERR"
  assert_lacks "(c1) never reported LIVE" "VERDICT: LIVE" "$OUT"
  # Both spellings the lost-tenant state has had: the pre-C1 describe_session_tenant
  # ("no tenant sent (") and describe_sent_tenant ("sent no tenant ("), on stdout
  # AND stderr — the old bug printed it on the LIVE line.
  assert_lacks "(c1) the session tenant was never lost (old spelling)" "no tenant sent (the session census" "$OUT$ERR"
  assert_lacks "(c1) the session tenant was never lost (new spelling)" "sent no tenant (the session census" "$OUT$ERR"
  printf '%s' "$TA" > "$MODE_DIR/default_tenant"

  echo "== (c1b) the ONLY barrier is the websocket answer's claim check (coord would accept any bearer)"
  # Runner A: its get_coord_device_token route is absent, so the cascade falls to
  # get_access_token_for_websocket, which never takes a tenant and returns A's
  # default slot - tenant A. The sibling B serves no mint of any kind: both mint
  # routes AND its websocket route 404, so the refusal asserted below can only be
  # A's answer. coord is a stub whose /mcp accepts every bearer, so a skipped
  # claim check would go LIVE.
  setmode runner absent; setmode nonce_runner absent; setmode census row; setmode row_tenant "$TB"
  printf 'absent' > "$MODE_DIR_B/runner"; printf 'absent' > "$MODE_DIR_B/nonce_runner"
  printf 'absent' > "$MODE_DIR_B/websocket"
  mkdir -p "$ROOT/sibling-b"
  printf '{"mcpServers":{"coord-mcp":{"type":"http","url":"%s/coord-mcp","headers":{"X-Coord-Mcp-Proxy-Key":"stale"}}}}\n' "$STUB_B" > "$ROOT/sibling-b/.mcp.json"
  run_case - "$STUB" QONTINUI_RUNNER_API_PORT="$PORT" QONTINUI_TERMINAL_ID=term-1 \
    COORD_REVIVE_NO_MINT= COORD_REVIVE_NO_BOOTSTRAP=1
  rm -rf "$ROOT/sibling-b"; rm -f "$MODE_DIR_B/websocket"
  assert_has   "(c1b) the websocket answer is refused by its claim" "source=runner-invoke:get_access_token_for_websocket -> RUNNER_MINT_WRONG_TENANT" "$ERR"
  assert_has   "(c1b) and the reason names the door, not a runner build" "get_access_token_for_websocket never takes a tenant" "$ERR"
  assert_lacks "(c1b) never LIVE" "VERDICT: LIVE" "$OUT"
fi

# ================================================================ the discharge
if [ "${MC_MUTANT:-0}" = "1" ]; then
  :
else
  MC_LIB="$REPO_ROOT/scripts/lib/mutation-control.sh"
  if [ -r "$MC_LIB" ]; then
    echo "== discharge: the assertions above are shown to be able to fail"
    # shellcheck source=/dev/null
    . "$MC_LIB"
    mc_ok()  { ok "$*"; }
    mc_bad() { bad "$*"; }
    mc_init ".claude/skills/coord-revive/tenant-arg-test.sh" "$SANDBOX"
    mkdir -p "$SANDBOX/ctl"
    cp "$SCRIPT" "$SANDBOX/ctl/coord-revive.sh"
    if MC_MUTANT=1 MC_SUBJECT="$SANDBOX/ctl/coord-revive.sh" bash "$0" >"$SANDBOX/ctl.log" 2>&1; then
      ok "staging control: an UNMUTATED lone copy runs GREEN"
    else
      bad "staging control: an UNMUTATED lone copy is already RED -- see $SANDBOX/ctl.log"
      tail -20 "$SANDBOX/ctl.log"
    fi
    mc_expect_red "never put the tenant on the invoke body" \
      "$SCRIPT" 's/^      MINVOKE_BODY="{\\"tenantId\\":\\"\$SESSION_TENANT\\"}"$/      MINVOKE_BODY="{}"/' \
      -- bash "$0"
    mc_expect_red "trust whatever token comes back, whichever tenant it claims" \
      "$SCRIPT" 's/^      if \[ "\$MCLAIM" != /      if false \&\& [ "$MCLAIM" != /' \
      -- bash "$0"
    mc_expect_red "never send the tenant to the nonce mint" \
      "$SCRIPT" 's/^      NTENANT_ARGS="--tenant \$SESSION_TENANT"$/      NTENANT_ARGS=""/' \
      -- bash "$0"
    mc_expect_red "call a minted nonce LIVE without reading its tenant back" \
      "$SCRIPT" 's/^      if \[ -z "\$SESSION_TENANT" \]; then$/      if true; then/' \
      -- bash "$0"
    mc_expect_red "collapse the nonce mint's tenant refusals into MINT_UNKNOWN" \
      "$SCRIPT" '/^      6)   NREF=/,/^           esac ;;$/d' \
      -- bash "$0"
    mc_expect_red "decode the claim without mapping base64url - and _" \
      "$SCRIPT" "s/ | cut -d. -f2 | tr '_-' '\\/+')\"\$/ | cut -d. -f2)\"/" \
      -- bash "$0"
    mc_expect_red "decode the claim without restoring base64 padding" \
      "$SCRIPT" '/^  \[ "\$pad" -gt 0 \] && seg=/d' \
      -- bash "$0"
    mc_expect_red "read a named tenant's null as RUNNER_SIGNED_OUT" \
      "$SCRIPT" 's/^                 \*get_coord_device_token) if \[ -n "\$SESSION_TENANT" \]; then$/                 *get_coord_device_token) if false; then/' \
      -- bash "$0"
    # Mutated to a DEAD port, never to the real default: a mutant that reached
    # 9876 would talk to whatever runner really runs on this box.
    mc_expect_red "ignore QONTINUI_RUNNER_API_PORT when choosing the runner" \
      "$SCRIPT" 's/^  \*) RUNNER_DEFAULT_ORIGIN=.*$/  *) RUNNER_DEFAULT_ORIGIN="${QONTINUI_RUNNER_URL:-http:\/\/127.0.0.1:1}" ;;/' \
      -- bash "$0"
    mc_expect_red "trust an L5 token whatever tenant it claims" \
      "$SCRIPT" 's/^            if \[ -n "\$SESSION_TENANT" \]; then$/            if false; then/' \
      -- bash "$0"
    mc_expect_red "re-read the census on the runner that answered the mint (C1)" \
      "$SCRIPT" '/^      # The session tenant was resolved ONCE, before the mint; nothing here re-reads it\.$/a\      SESSION_TENANT_DONE=""; SESSION_TENANT=""; SESSION_TENANT_SRC=""; RUNNER_DEFAULT_ORIGIN="${NURL%/coord-mcp}"; resolve_session_tenant' \
      -- bash "$0"
    mc_expect_red "check the invoke token's claim only for get_coord_device_token (W1)" \
      "$SCRIPT" 's/^    if \[ -n "\$SESSION_TENANT" \]; then$/    if [ -n "$SESSION_TENANT" ] \&\& [ "$MCMD" = get_coord_device_token ]; then/' \
      -- bash "$0"
    mc_expect_red "never put the tenant on the L5 credential body" \
      "$SCRIPT" '/^    \[ -n "\$SESSION_TENANT" \] && BOOT_REQ=/d' \
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
