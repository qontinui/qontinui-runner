#!/usr/bin/env bash
# Self-test for set-label.sh — the thin client of coord's ONE label door.
#
# Runs the REAL script. Hermetic: `curl` is PATH-shadowed by a stub that records
# every request (method, URL, header, body) and answers from a per-case fixture;
# `gh` is PATH-shadowed by a stub that FAILS LOUDLY, because this client must
# never call gh at all — the two-step gh-then-coord shape is the half-write the
# door exists to remove. No network, no credential, no runner.
#
# What is pinned, and why each matters:
#   - the request SHAPE: POST for declare, DELETE for --unset, `mode: replace`
#     under --replace, `dry_run: true` under --dry-run, the labels array
#     verbatim (no local validation — a rejected label must reach coord and
#     come back in `rejected[]`, or the client is re-growing the mirror that
#     drifted five times);
#   - the CASCADE: a proxy-shaped .mcp.json beside $PWD is rung 1 and its nonce
#     header is sent; a 401 there falls through to rung 2 with $COORD_AGENT_JWT;
#     a runner-shaped 404 (old runner, no forwarder route) falls through too,
#     while coord's typed repo-not-in-tenant 404 is an answer; with neither rung
#     the exit is 4 and the message says nothing was written;
#   - the VERDICT: `rejected[]` non-empty ⇒ exit 1 with a `rejected:` line per
#     label; a clean declare ⇒ exit 0 with one `ok:` line per GitHub add;
#   - and that gh is NEVER invoked on any path.
#
# The PR coordinates are deliberately unresolvable (`--pr 0` on a repo that does
# not exist) so that if the stubs were ever bypassed the real call could not
# succeed either.

set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$HERE/set-label.sh"

FAILURES=0
CASES=0
pass() { CASES=$((CASES + 1)); echo "ok: $*"; }
fail() { CASES=$((CASES + 1)); FAILURES=$((FAILURES + 1)); echo "FAIL: $*" >&2; }

[[ -f "$SCRIPT" ]] || { echo "FAIL: $SCRIPT not found" >&2; exit 1; }
bash -n "$SCRIPT" || { echo "FAIL: set-label.sh does not parse" >&2; exit 1; }

WORK="$(mktemp -d)" || { echo "FAIL: mktemp -d failed" >&2; exit 1; }
trap 'rm -rf "$WORK"' EXIT
STUBS="$WORK/stubs"; mkdir -p "$STUBS" "$WORK/cwd" "$WORK/home"
LOG="$WORK/requests.log"
: > "$LOG"

# --- curl stub: records the request, answers from $STUB_CASE ------------------
cat > "$STUBS/curl" <<'EOF'
#!/usr/bin/env bash
# Records: METHOD URL HEADER BODY (one line, tab-separated), then answers from
# the fixture directory named by $STUB_FIXTURES, keyed on the URL's host.
method="GET"; url=""; hdr=""; body=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    -X) method="$2"; shift 2 ;;
    -H) case "$2" in Content-Type:*) : ;; *) hdr="$2" ;; esac; shift 2 ;;
    -d) body="$2"; shift 2 ;;
    -w|-m) shift 2 ;;
    -sS|-s|-S) shift ;;
    http://*|https://*) url="$1"; shift ;;
    *) shift ;;
  esac
done
printf '%s\t%s\t%s\t%s\n' "$method" "$url" "$hdr" "$body" >> "$STUB_LOG"
case "$url" in
  http://127.0.0.1:9876/coord-mcp/pr-labels)  fx="$STUB_FIXTURES/forwarder" ;;
  https://coord.example.test/coord/pr-labels) fx="$STUB_FIXTURES/direct" ;;
  *) fx="" ;;
esac
if [[ -z "$fx" || ! -f "$fx.code" ]]; then
  echo "curl: (7) Failed to connect" >&2
  exit 7
fi
cat "$fx.body"; printf '\n%s' "$(cat "$fx.code")"
exit 0
EOF
chmod +x "$STUBS/curl"

# --- gh stub: must never run -------------------------------------------------
cat > "$STUBS/gh" <<'EOF'
#!/usr/bin/env bash
echo "gh-stub: set-label.sh must not call gh (args: $*)" >> "$STUB_LOG"
echo "FAIL: gh was invoked" >&2
exit 99
EOF
chmod +x "$STUBS/gh"

# A proxy-shaped .mcp.json beside the cwd — rung 1.
cat > "$WORK/cwd/.mcp.json" <<'EOF'
{"mcpServers":{"coord-mcp":{"url":"http://127.0.0.1:9876/coord-mcp","headers":{"Authorization":"Bearer test-nonce-123"}}}}
EOF

FIX="$WORK/fx"; mkdir -p "$FIX"
set_fixture() { # $1 rung (forwarder|direct) $2 code $3 body
  printf '%s' "$2" > "$FIX/$1.code"; printf '%s' "$3" > "$FIX/$1.body"
}
clear_fixtures() { rm -f "$FIX"/*.code "$FIX"/*.body; : > "$LOG"; }

run_client() { # args → stdout/stderr captured to files; echoes rc
  (
    cd "$WORK/cwd" && \
    HOME="$WORK/home" PATH="$STUBS:$PATH" STUB_LOG="$LOG" STUB_FIXTURES="$FIX" \
    COORD_HTTP_URL="https://coord.example.test" \
    bash "$SCRIPT" "$@" >"$WORK/out" 2>"$WORK/err"
  )
  echo $?
}
out() { cat "$WORK/out"; }
err() { cat "$WORK/err"; }
last_req() { tail -n 1 "$LOG"; }
req_count() { wc -l < "$LOG" | tr -d ' '; }
gh_invoked() { grep -q '^gh-stub' "$LOG"; }

ok_body='{"tenant_id":"t","repo":"qontinui/does-not-exist","pr_number":0,"mode":"merge","dry_run":false,"valid":["coord:downstream-of=qontinui/x#1"],"written":1,"deleted":0,"rejected":[],"github":{"added":["coord:downstream-of=qontinui/x#1"],"removed":[]}}'

# ---- 1. a clean declare goes to rung 1 with the nonce, as POST, labels verbatim
clear_fixtures; set_fixture forwarder 200 "$ok_body"
rc=$(run_client --repo qontinui/does-not-exist --pr 0 --label 'coord:downstream-of=x#1')
req=$(last_req)
[[ "$rc" == 0 ]] && pass "clean declare exits 0" || fail "clean declare rc=$rc; err: $(err)"
[[ "$req" == $'POST\thttp://127.0.0.1:9876/coord-mcp/pr-labels\tAuthorization: Bearer test-nonce-123\t'* ]] \
  && pass "rung 1 is the forwarder with the .mcp.json nonce, method POST" || fail "unexpected request: $req"
[[ "$req" == *'"labels": ["coord:downstream-of=x#1"]'* || "$req" == *'"labels":["coord:downstream-of=x#1"]'* ]] \
  && pass "labels travel verbatim (no local validation)" || fail "labels not verbatim: $req"
[[ "$req" == *'"mode": "merge"'* || "$req" == *'"mode":"merge"'* ]] && pass "default mode is merge" || fail "mode missing: $req"
out | grep -q 'ok: declared "coord:downstream-of=qontinui/x#1"' && pass "renders one ok: line per GitHub add" || fail "render: $(out)"
[[ "$(req_count)" == 1 ]] && pass "exactly one request" || fail "expected 1 request, got $(req_count)"
gh_invoked && fail "gh was invoked" || pass "gh never invoked (declare)"

# ---- 2. --replace + --dry-run flags reach the body
clear_fixtures; set_fixture forwarder 200 '{"repo":"r","pr_number":0,"dry_run":true,"valid":["coord:blocked"],"written":0,"deleted":0,"rejected":[],"github":{"added":[],"removed":[]}}'
rc=$(run_client --repo qontinui/does-not-exist --pr 0 --label coord:blocked --replace --dry-run)
req=$(last_req)
[[ "$rc" == 0 ]] && pass "dry run exits 0" || fail "dry run rc=$rc"
[[ "$req" == *'"mode": "replace"'* || "$req" == *'"mode":"replace"'* ]] && pass "--replace sends mode=replace" || fail "mode: $req"
[[ "$req" == *'"dry_run": true'* || "$req" == *'"dry_run":true'* ]] && pass "--dry-run sends dry_run=true" || fail "dry_run: $req"
out | grep -q 'dry run' && pass "dry run says so" || fail "dry-run render: $(out)"

# ---- 3. --replace with no labels is a legal total retraction (empty array)
clear_fixtures; set_fixture forwarder 200 '{"repo":"r","pr_number":0,"dry_run":false,"valid":[],"written":0,"deleted":2,"rejected":[],"github":{"added":[],"removed":["coord:blocked","coord:experimental"]}}'
rc=$(run_client --repo qontinui/does-not-exist --pr 0 --replace)
req=$(last_req)
[[ "$rc" == 0 ]] && pass "total retraction exits 0" || fail "total retraction rc=$rc; $(err)"
[[ "$req" == *'"labels": []'* || "$req" == *'"labels":[]'* ]] && pass "empty labels array posted under replace" || fail "labels: $req"
out | grep -c 'ok: retracted' | grep -q '^2$' && pass "renders one line per retraction" || fail "retraction render: $(out)"

# ---- 4. --unset is a DELETE with {repo, pr_number, label}
clear_fixtures; set_fixture forwarder 200 '{"repo":"qontinui/does-not-exist","pr_number":0,"label":"coord:blocked","deleted":true}'
rc=$(run_client --repo qontinui/does-not-exist --pr 0 --unset coord:blocked)
req=$(last_req)
[[ "$rc" == 0 ]] && pass "unset exits 0" || fail "unset rc=$rc"
[[ "$req" == $'DELETE\t'* && ( "$req" == *'"label": "coord:blocked"'* || "$req" == *'"label":"coord:blocked"'* ) ]] \
  && pass "--unset sends DELETE with the label" || fail "unset request: $req"
gh_invoked && fail "gh was invoked (unset)" || pass "gh never invoked (unset)"

# ---- 5. rejected[] ⇒ exit 1, a rejected: line per label, nothing else claimed
clear_fixtures; set_fixture forwarder 422 '{"repo":"r","pr_number":0,"dry_run":false,"valid":[],"written":0,"deleted":0,"rejected":[{"label":"coord:nope","reason":"unknown coord:* label key"}],"github":{"added":[],"removed":[]}}'
rc=$(run_client --repo qontinui/does-not-exist --pr 0 --label coord:nope)
[[ "$rc" == 1 ]] && pass "all-rejected exits 1" || fail "rejected rc=$rc"
err | grep -q 'rejected: "coord:nope" — unknown coord:\* label key' && pass "rejected line names the label and coord's reason" || fail "rejected render: $(err)"
out | grep -q 'ok: declared' && fail "claimed a declare that did not happen" || pass "no false ok: line"

# ---- 6. a 401 from the forwarder falls through to the direct rung with the agent JWT
clear_fixtures; set_fixture forwarder 401 '{"error":"dead nonce"}'; set_fixture direct 200 "$ok_body"
rc=$( cd "$WORK/cwd" && HOME="$WORK/home" PATH="$STUBS:$PATH" STUB_LOG="$LOG" STUB_FIXTURES="$FIX" COORD_HTTP_URL="https://coord.example.test" COORD_AGENT_JWT="agent.jwt.here" bash "$SCRIPT" --repo qontinui/does-not-exist --pr 0 --label coord:blocked >"$WORK/out" 2>"$WORK/err"; echo $? )
[[ "$rc" == 0 ]] && pass "401 on rung 1 falls through and rung 2 answers" || fail "fallthrough rc=$rc; $(err)"
[[ "$(req_count)" == 2 ]] && pass "two requests: forwarder then direct" || fail "expected 2 requests, got $(req_count): $(cat "$LOG")"
last_req | grep -q $'^POST\thttps://coord.example.test/coord/pr-labels\tAuthorization: Bearer agent.jwt.here' && pass "direct rung uses \$COORD_AGENT_JWT against \$COORD_HTTP_URL/coord/pr-labels" || fail "direct request: $(last_req)"

# ---- 7. a runner-shaped 404 (no forwarder route on an old runner) falls through; coord's typed 404 is an answer
clear_fixtures; set_fixture forwarder 404 '{"success":false,"error":"not found"}'; set_fixture direct 200 "$ok_body"
rc=$( cd "$WORK/cwd" && HOME="$WORK/home" PATH="$STUBS:$PATH" STUB_LOG="$LOG" STUB_FIXTURES="$FIX" COORD_HTTP_URL="https://coord.example.test" COORD_DEVICE_JWT="device.jwt" bash "$SCRIPT" --repo qontinui/does-not-exist --pr 0 --label coord:blocked >"$WORK/out" 2>"$WORK/err"; echo $? )
[[ "$rc" == 0 && "$(req_count)" == 2 ]] && pass "runner-shaped 404 falls through to the direct rung" || fail "old-runner 404: rc=$rc reqs=$(req_count)"
clear_fixtures; set_fixture forwarder 404 '{"error":"repo_not_found_in_tenant_scope","repo":"qontinui/does-not-exist"}'
rc=$(run_client --repo qontinui/does-not-exist --pr 0 --label coord:blocked)
[[ "$rc" == 1 && "$(req_count)" == 1 ]] && pass "coord's typed 404 is an answer (no fallthrough), exit 1" || fail "typed 404: rc=$rc reqs=$(req_count) err=$(err)"
err | grep -q 'repo_not_found_in_tenant_scope' && pass "typed 404 body is shown" || fail "typed 404 render: $(err)"

# ---- 8. no rung answers ⇒ exit 4 and 'NOTHING was written'
clear_fixtures   # no fixtures: the stub curl fails to connect everywhere
rc=$(run_client --repo qontinui/does-not-exist --pr 0 --label coord:blocked)
[[ "$rc" == 4 ]] && pass "no door ⇒ exit 4" || fail "no-door rc=$rc"
err | grep -q 'NOTHING was written' && pass "no-door message says nothing was written" || fail "no-door render: $(err)"
err | grep -q 'coord_pr_label_set' && pass "no-door message points at the MCP tool" || fail "no MCP pointer: $(err)"

# ---- 9. usage errors exit 2 before any request
clear_fixtures
rc=$(run_client --repo qontinui/does-not-exist --pr 0)
[[ "$rc" == 2 && "$(req_count)" == 0 ]] && pass "nothing to do ⇒ exit 2, no request" || fail "usage rc=$rc reqs=$(req_count)"
rc=$(run_client --repo qontinui/does-not-exist --pr abc --label coord:blocked)
[[ "$rc" == 2 ]] && pass "non-integer --pr ⇒ exit 2" || fail "bad pr rc=$rc"
rc=$(run_client --repo qontinui/does-not-exist --pr 0 --label coord:blocked --unset coord:blocked)
[[ "$rc" == 2 ]] && pass "--label and --unset together ⇒ exit 2" || fail "exclusive rc=$rc"

# ---- 10. the script itself carries no localhost:9870 default and no gh call
grep -q 'localhost:9870\|127\.0\.0\.1:9870' "$SCRIPT" && fail "set-label.sh still names the dead :9870 default" || pass "no :9870 default in set-label.sh"
grep -Eq '^[^#]*\bgh (api|pr|label)\b' "$SCRIPT" && fail "set-label.sh still calls gh" || pass "set-label.sh has no gh call"

echo
if [[ "$FAILURES" -gt 0 ]]; then
  echo "set-label-selftest: $FAILURES of $CASES assertion(s) FAILED" >&2
  exit 1
fi
echo "set-label-selftest: PASS $CASES assertions, 0 failures"
