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
#   (n7) no tenant known                         -> body {cwd} only, no identity read, LIVE
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
STUB_PID=""
cleanup() { [ -n "$STUB_PID" ] && kill "$STUB_PID" 2>/dev/null; rm -rf "$SANDBOX"; }
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
    return "%s.%s.sig" % (b64({"alg": "none"}), b64({"sub_type": "device", "tenant_id": tenant, "exp": exp}))


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
            elif census == "no-tenancy":
                pass
            else:
                self._send(404, {"error": "not found"})
                return
            other = {"identity": {"terminalId": "term-other"}, "tenancy": {"row": {"tenantId": "dddddddd-0000-4000-8000-00000000000d"}}}
            self._send(200, {"success": True, "data": {"status": "ok", "reason": None, "sessions": [other, row]}})
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
            elif runner == "p3-multi":
                if asked:
                    self._send(200, {"success": True, "data": token(asked)})
                else:
                    self._send(409, {"success": False, "error": "invoke proxy: in-process invoke of 'get_coord_device_token' refused: get_coord_device_token:tenant_required: this runner holds coord credentials for 2 tenants"})
            else:  # p3-single
                self._send(200, {"success": True, "data": token(asked or mode("default_tenant"))})
            return
        if self.path == "/ui-bridge/invoke/get_access_token_for_websocket":
            self._send(200, {"success": True, "data": token(mode("default_tenant"))})
            return
        if self.path == "/mcp":
            rid = req.get("id", 1)
            self._send(200, {"jsonrpc": "2.0", "id": rid, "result": {"tools": [{"name": "coord_query_identity"}, {"name": "coord_memory_search"}]}})
            return
        if self.path == "/agents/credential":
            if not req.get("tenant_id") and mode("coord", "live") == "live":
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
  OUT="$(cd "$cwd" && env -u QONTINUI_TENANT_ID -u QONTINUI_TERMINAL_ID -u COORD_DEVICE_JWT -u COORD_AGENT_JWT \
        HOME="$FAKE_HOME" USERPROFILE="$FAKE_HOME" CLAUDE_CONFIG_DIR="$FAKE_HOME/cc" \
        QONTINUI_ROOT="$ROOT" QONTINUI_RUNNER_URL="$runner" COORD_HTTP_URL="$coord" \
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
assert_eq "(n7) only the probe's own end-to-end identity call, no tenant read-back" "1" "$(identity_calls)"
assert_has "(n7) VERDICT LIVE over the minted nonce" "transport=loopback-proxy-minted" "$OUT"
setmode nonce_runner absent

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
