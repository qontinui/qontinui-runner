#!/usr/bin/env bash
# coord-tenant-credential.sh -- stage a coord device credential for the tenant
# PROVEN to own a repo, or say UNKNOWN. SOURCE it; it runs nothing on its own.
#
# Plan: 2026-09-23-ccfg-scripts-mint-device-credentials-with-no-tenant, Phase 1.
#
# WHY. A device bound to several tenants that mints `POST /agents/credential`
# with only `{device_id}` gets a token for the device's LEGACY POINTER tenant
# (coord agent_worktrees::resolve_device_tenant, sole binding -> pointer; the
# repo-derived arm is still `shadow`). Measured 2026-09-23 on merytshost: that
# token claimed meryts-2-0 while every qontinui/* repo belongs to qontinui, so a
# tenant-scoped read came back EMPTY and rendered as "nothing" (coord finding
# 26594956; #1104, #1121). This file is the one place that answers "which of
# this device's tenants owns <owner/repo>", so each caller stops carrying its
# own copy.
#
# THE PROOF. `GET /pr-merge/<owner%2Fname>/0/author-session` (coord
# pr_merge::get_author_session) answers 200 for ANY pr number when the CALLER's
# tenant owns the repo and ONE 404 for "not yours" and "no such repo". So under
# a token whose `tenant_id` claim is T: 200 PROVES T owns the repo, 404 REFUTES
# it, anything else says nothing about T (the door failed) and stops the walk.
# Owner/name is case-sensitive and a bare name 404s: callers pass the exact
# slug (ctc_origin_slug reads it off a checkout's origin).
#
# CALLER CONTRACT (set before calling; all but CTC_TMP have defaults)
#   CTC_TMP        a private (0700) directory this file may write into. It has
#                  NO default: every function that writes under it (itself or
#                  through a helper) REFUSES when it is unset, empty, not a
#                  directory or not writable, before any file is written or
#                  any request is sent, and says so in its own failure
#                  convention -- ctc_stage_for_tenant prints rejected(<why>);
#                  ctc_owner_tenant, ctc_prove_tenant and ctc_expected_tenant
#                  end `unknown` with the reason in their note
#                  (ctc_expected_tenant still answers `invalid` for a malformed
#                  argument first); ctc_bindings refuses a FRESH read, leaving
#                  the list UNKNOWN (not cached) with the reason in
#                  CTC_BINDINGS_NOTE, but still serves a read it already
#                  cached; ctc_bindings_over and ctc_drop_runner_token do
#                  nothing and return 0. Without it every staged path resolved
#                  to the filesystem root (measured 2026-11-09:
#                  `/bearer.hdr: Permission denied`; run as root, a bearer
#                  would have been staged at /bearer.hdr).
#   CTC_COORD_URL  coord base (default $COORD_HTTP_URL, else https://coord.qontinui.io).
#   CTC_CURL       curl binary or a test stub (default curl).
#   CTC_PY         a Python 3 (default python3).
#   CTC_TIMEOUT    seconds per HTTP call (default 30).
#   CTC_CONNECT_TIMEOUT  seconds to connect, per HTTP call (default 10).
#   CTC_DEVICE     device id (default $QONTINUI_MACHINE_ID, else
#                  ~/.qontinui/machine.json device_id / machine_id).
#   CTC_NO_MINT=1  never mint at coord -- neither the anonymous POST
#                  /agents/credential nor the per-binding POST /coord/devices/
#                  <device_id>/tenant-token -- so only a static token or the
#                  local runner's (below) claiming the tenant is used. A token
#                  this CTC_TMP already holds from either mint is still
#                  re-served (mint(cached), binding(cached)): the flag stops a
#                  NEW request, not the reuse of one this run already made.
#   CTC_NO_ANON_MINT=1  never make the ANONYMOUS POST /agents/credential mint
#                  -- neither the stage cascade's last rung nor ctc_bindings'
#                  tenant-less read, which is that same mint -- while the
#                  per-binding door still runs. For a caller whose own door
#                  refuses the bootstrap token that mint returns (coord's
#                  POST /coord/verification-runs: scripts/verification-outcome-
#                  flush.sh). CTC_NO_MINT=1 is the stricter flag and wins: with
#                  both set, neither door is asked and the reason reads
#                  `minting is disabled`. A token this CTC_TMP already holds is
#                  still re-served, as under CTC_NO_MINT. Only the exact value
#                  1 enables either flag.
#   CTC_NO_RUNNER=1  never ask the local runner (hermetic suites on a dev box).
#                  The runner is also never asked when the coord url is not
#                  https://coord.qontinui.io: its token is minted for that coord.
#   QONTINUI_RUNNER_PORT  the local runner's port (digits 1-65535, else 9876).
#   CTC_RUNNER_CONNECT_TIMEOUT / CTC_RUNNER_TIMEOUT  the runner call's bounds
#                  (default 3 / 15 s; a loopback call stays fast whatever the
#                  caller's CTC_CONNECT_TIMEOUT / CTC_TIMEOUT for coord are).
# Bearers are staged in 0600 header files and passed as `curl -H @file`, never
# on argv, and are only ever sent to an https or loopback coord.
#
# FUNCTIONS
#   ctc_stage_for_tenant <tenant>  -> writes $CTC_TMP/bearer.hdr; prints its
#       source (env|file|runner|runner(cached)|binding|binding(cached)|mint|
#       mint(cached)) or `rejected(<why>)` and writes nothing. Only a token
#       whose `tenant_id` claim IS <tenant> is staged. Order: $COORD_DEVICE_JWT,
#       the file token, the local runner's UI Bridge mint (POST
#       http://127.0.0.1:<port>/ui-bridge/invoke/get_coord_device_token
#       {"args":{"tenantId":<tenant>}}), coord's per-binding door (POST
#       /coord/devices/<device_id>/tenant-token {"tenant_id":<tenant>},
#       presenting the first STATIC token -- env, then file -- that is a fresh
#       device JWT with a user_id, for THIS device when its id resolves,
#       claiming ANOTHER tenant;
#       its answer is staged only when it is a fresh device token for <tenant>
#       and that device), then the anonymous POST /agents/credential mint,
#       whose bootstrap token coord refuses for every write outside its
#       closeout allowlist. A `binding` token is a PAIRED device JWT (coord
#       jwt.rs issue_device): callers class it with `runner`, never with a
#       bootstrap `mint` -- but unlike `runner`, a 401 to it is neither dropped
#       nor retried (the walkers below re-stage only a `runner*` source).
#   ctc_drop_runner_token <tenant> <why> -> forget the runner's token for
#       <tenant> and refuse the runner rung for it for the rest of the run (a
#       caller whose coord answered 401 to it re-stages and reaches the
#       per-binding door, then the mint).
#   ctc_bindings                   -> CTC_BINDINGS (space-separated tenant ids)
#       or empty with CTC_BINDINGS_NOTE; CTC_BINDING_SLUGS (space-separated
#       `<uuid>=<slug>` pairs, only for a served slug that is a plain
#       [A-Za-z0-9._-] word); CTC_BINDINGS_COUNT (how many bindings coord says
#       the device has, dropped entries included, or empty when UNKNOWN); and
#       CTC_BINDINGS_MULTI=1 when the device is PROVEN bound to two or more
#       tenants -- by the list itself, or by the tenant-less mint answering 422
#       `tenant_ambiguous` (coord's `live` resolution mode refuses exactly the
#       multi-bound device; that answer carries a COUNT and no ids, so the list
#       stays UNKNOWN while the multiplicity is proven). Call directly, never in
#       $(...).
#       The slugs come from that identity read; when the tenant-less mint is
#       refused (422) there is no read, so every slug is UNKNOWN until
#       ctc_bindings_over is given a credential the caller already holds.
#       Under CTC_NO_MINT=1 or CTC_NO_ANON_MINT=1 the list is not read at all
#       (the tenant-less read IS the anonymous mint): it stays UNKNOWN, with
#       the flag named in CTC_BINDINGS_NOTE, and is cached as unread for the
#       shell. A caller holding a static device credential reads it with
#       ctc_bindings_over instead.
#   ctc_bindings_over <bearer-header-file> -> the same list and slugs, read
#       over that (tenant-scoped) bearer; fills the fields only on a readable
#       answer, otherwise leaves them as they were. ctc_expected_tenant calls it
#       with the proven owner's bearer.
#   ctc_slug_of <tenant>           -> prints the slug the last identity read
#       named for that tenant, or nothing. PURE: it never reads the network.
#   ctc_tenant_label <tenant>      -> prints `<slug> (<uuid>)`, or
#       `slug UNKNOWN (<uuid>)`. PURE.
#   ctc_expected_tenant [--tenant <uuid>] [--owner-of <owner/repo>] [--no-slug]
#       -> the tenant a caller should ACT FOR, resolved in this order: the
#       --tenant value; $QONTINUI_TENANT_ID; the tenant PROVEN to own
#       --owner-of (ctc_owner_tenant); the device's SOLE binding. Sets
#       CTC_EXPECTED_STATE resolved|required|unknown|invalid,
#       CTC_EXPECTED_TENANT, CTC_EXPECTED_SOURCE, CTC_EXPECTED_SLUG and
#       CTC_EXPECTED_NOTE. `required` means the device is PROVEN multi-bound and
#       nothing named a tenant; its note is the typed `TENANT_REQUIRED: ...`
#       line. `unknown` means the bindings could not be read, so whether a
#       tenant is required is itself UNKNOWN. `invalid` is a malformed --tenant
#       or $QONTINUI_TENANT_ID: an error, never silently ignored. A caller with
#       its own step between the env var and the sole binding (coord-revive's
#       session census) resolves that step itself and passes it as --tenant.
#       Call directly, never in $(...).
#   ctc_owner_tenant <owner/repo>... -> CTC_STATE proven|refuted|unknown,
#       CTC_TENANT (proven only), CTC_NOTE, CTC_BEARER_SRC, CTC_TRANSPORT (1
#       when a not-proven answer met an unreachable coord); on `proven` the
#       tenant's bearer is left at $CTC_TMP/bearer.hdr. Every repo named must be
#       owned by the SAME tenant. Call directly, never in $(...).
#   ctc_prove_tenant <tenant> <owner/repo> -> the SAME proof for ONE named
#       tenant (no candidate walk): CTC_STATE proven|refuted|unknown, CTC_NOTE,
#       CTC_BEARER_SRC, CTC_DOOR_CODE (the ownership door's answer, or empty
#       when no request went out). For a caller that already knows which tenant
#       it is about to act under and must prove only that one owns the repo.
#       Call directly, never in $(...).
#   ctc_origin_slug <checkout-dir> -> prints owner/name from the RAW
#       remote.origin.url (GitHub https/ssh), or nothing. Never guesses.
#   ctc_is_uuid <s>
#
# refuted and unknown are BOTH "not proven": a caller renders UNKNOWN and acts
# under no tenant. They differ only in what the note can say.

# The Python 3 every helper below runs: $CTC_PY, else `python3`, else a Python 3
# spelled `python` (Git Bash on Windows ships only that spelling).
# Resolved by RUNNING it, not by `command -v`: on Windows a `python3` on PATH
# is often the Store alias, which exists and does not run. One start, at load.
if [ -z "${CTC_PY:-}" ]; then
  for _ctc_c in python3 python; do
    if "$_ctc_c" -c 'import sys; sys.exit(0 if sys.version_info[0] == 3 else 1)' >/dev/null 2>&1; then
      CTC_PY="$_ctc_c"; break
    fi
  done
  unset _ctc_c
fi

# ctc_sanitize <text...> -> the text with a tab turned into a space and every
# other C0 control, DEL and every byte
# >= 0x80 (which covers C1 controls in any encoding) REPLACED by `?` -- kept
# visible, never silently dropped -- and bounded to 2000 bytes with an explicit
# `...[truncated]`. Coord-served values are passed through it wherever this
# file BUILDS a message, so a served ESC / OSC / BEL sequence can never reach a
# terminal. No pipe into head: the result never depends on SIGPIPE.
ctc_sanitize() {
  local _s
  _s="$(printf '%s' "$*" | LC_ALL=C tr '\011' ' ' | LC_ALL=C tr '\000-\037\177-\377' '?')"
  [ "${#_s}" -le 2000 ] || _s="${_s:0:2000}...[truncated]"
  printf '%s' "$_s"
}

ctc_is_uuid() {
  local re='^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$'
  [[ "${1:-}" =~ $re ]]
}

# _ctc_tmp_ok -> 0 iff $CTC_TMP names a directory this file can write into;
# otherwise 1, with _CTC_TMP_WHY saying what is wrong and naming the CALLER
# CONTRACT. Every public function that writes under $CTC_TMP calls it before
# any write or request and refuses in its own failure convention (see the
# header). Read through
# ${CTC_TMP:-} / ${CTC_TMP+set}, never a bare "$CTC_TMP", so a caller running
# under `set -u` reaches the refusal instead of an unbound-variable exit.
_CTC_TMP_WHY=""
_ctc_tmp_ok() {
  local d="${CTC_TMP:-}" why
  if [ -z "${CTC_TMP+set}" ]; then why="CTC_TMP is unset"
  elif [ -z "$d" ]; then why="CTC_TMP is empty"
  elif [ ! -d "$d" ]; then why="CTC_TMP ($(ctc_sanitize "$d")) is not a directory"
  elif [ ! -w "$d" ] || [ ! -x "$d" ]; then why="CTC_TMP ($(ctc_sanitize "$d")) is not writable"
  else _CTC_TMP_WHY=""; return 0; fi
  _CTC_TMP_WHY="$why -- the CALLER CONTRACT of coord-tenant-credential.sh requires CTC_TMP to name a private (0700) directory this file may write into; refused before any file was written or request sent"
  return 1
}

_ctc_np() { # a POSIX path as the native curl on this box wants it
  if declare -F native_path_w >/dev/null 2>&1; then native_path_w "$1"
  elif command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"
  else printf '%s' "$1"; fi
}

_ctc_url() { local u="${CTC_COORD_URL:-${COORD_HTTP_URL:-https://coord.qontinui.io}}"; printf '%s' "${u%/}"; }

# A device JWT is never sent in clear text to an arbitrary host.
_ctc_url_ok() {
  local u re_https re_loop
  u="$(_ctc_url)"
  re_https='^https://[A-Za-z0-9.-]+(:[0-9]+)?(/[^@]*)?$'
  re_loop='^http://(127\.0\.0\.1|localhost|\[::1\])(:[0-9]+)?(/[^@]*)?$'
  [[ "$u" =~ $re_https ]] || [[ "$u" =~ $re_loop ]]
}

_ctc_shaped() {
  case "$1" in "" | *[!A-Za-z0-9._-]*) return 1 ;; esac
  local dots="${1//[!.]/}"   # pure bash: callers may run under a minimal PATH
  [ "${#dots}" = 2 ]
}

# jwt-cascade-selection: a static token is selected on VALIDITY, never presence
# -- shaped, `exp` more than 60 s away, AND its tenant_id claim equal to the
# tenant asked for (`_ctc_usable_for`), so a stale or other-tenant
# $COORD_DEVICE_JWT falls through to the file and then to the mint.
_ctc_usable_for() { # <jwt> <tenant>
  _ctc_shaped "$1" || return 1
  _ctc_facts_ok "$(_ctc_facts "$1")" "$2"
}

# _ctc_facts_ok "<exp> <tenant_id>" <tenant> -> 0 iff exp is more than 60 s
# away AND the tenant_id claim IS <tenant> (case-folded).
_ctc_facts_ok() {
  local exp="${1%% *}" claim="${1#* }"
  exp="${exp%%.*}"
  [[ "$exp" =~ ^[0-9]+$ ]] || return 1
  (( exp - $(date +%s) > 60 )) || return 1
  [ -n "$claim" ] && [ "${claim,,}" = "${2,,}" ]
}

# _ctc_facts <jwt> -> "<exp> <tenant_id>" (either may be empty) in ONE
# interpreter start: this runs for every candidate token, and a Python start
# per claim doubled the cost of every caller's run.
_ctc_facts() {
  H_JWT="$1" "${CTC_PY:-python3}" - <<'PY' 2>/dev/null | tr -d '\r' || true
import base64, json, os
try:
    seg = os.environ["H_JWT"].split(".")[1]
    seg += "=" * (-len(seg) % 4)
    d = json.loads(base64.urlsafe_b64decode(seg))
    e, t = d.get("exp"), d.get("tenant_id")  # envelope-ok: JWT claims, not a fleet response envelope
    e = str(int(e)) if isinstance(e, (int, float)) and not isinstance(e, bool) else ""
    print(e + " " + (t if isinstance(t, str) else ""))
except Exception:
    print(" ")
PY
}

_ctc_device() {
  local home_dir="${HOME:-${USERPROFILE:-}}" mf
  if [ -n "${CTC_DEVICE:-}" ]; then printf '%s' "$CTC_DEVICE"; return 0; fi
  if [ -n "${QONTINUI_MACHINE_ID:-}" ]; then printf '%s' "$QONTINUI_MACHINE_ID"; return 0; fi
  mf="$home_dir/.qontinui/machine.json"
  [ -n "$home_dir" ] && [ -r "$mf" ] || return 0
  _ctc_machine_json_field "$mf" device_id machine_id
}

_ctc_machine_json_field() { # <file> <key>... -> the first non-empty string value
  # Read with grep, not an interpreter: this runs on every caller's hot path.
  # machine.json is a flat object of uuid/hostname strings, so the first
  # `"key": "value"` pair is exact.
  local k v f="$1"; shift
  for k in "$@"; do
    v="$(grep -o "\"$k\"[[:space:]]*:[[:space:]]*\"[^\"]*\"" "$f" 2>/dev/null | head -1 | sed 's/.*:[[:space:]]*"//; s/"$//' | tr -d '\r[:space:]')"
    [ -n "$v" ] && { printf '%s' "$v"; return 0; }
  done
  return 0
}

# _ctc_mint <tenant-or-empty> <outfile> -> prints the HTTP code; the body lands
# in <outfile> (0600).
_ctc_mint() {
  local dev body
  dev="$(_ctc_device)"
  [ -n "$dev" ] || { printf 'nodev'; return 0; }
  if ctc_is_uuid "$dev" && { [ -z "$1" ] || ctc_is_uuid "$1"; }; then
    # Both are uuids, so no character in them needs JSON escaping.
    # json.dumps's spelling (", " and ": "), so the body reads the same
    # whichever arm built it.
    if [ -n "$1" ]; then body="{\"device_id\": \"$dev\", \"tenant_id\": \"$1\"}"; else body="{\"device_id\": \"$dev\"}"; fi
  else
    body="$(H_DEV="$dev" H_T="$1" "${CTC_PY:-python3}" -c 'import json,os
b={"device_id":os.environ["H_DEV"]}
if os.environ.get("H_T"): b["tenant_id"]=os.environ["H_T"]
print(json.dumps(b))' | tr -d '\r')"
  fi
  ( umask 077; : > "$2" )
  local code
  code="$("${CTC_CURL:-curl}" -sS -o "$(_ctc_np "$2")" -w '%{http_code}' --connect-timeout "${CTC_CONNECT_TIMEOUT:-10}" -m "${CTC_TIMEOUT:-30}" \
    -X POST "$(_ctc_url)/agents/credential" -H 'Content-Type: application/json' -d "$body" 2>/dev/null)" || code=000
  _ctc_mark "${code:-000}"
  printf '%s' "${code:-000}"
}

# _ctc_mark <http-code> -- a 000 is a TRANSPORT failure (coord unreachable),
# recorded on disk because several callers run in $(...). ctc_owner_tenant
# surfaces it as CTC_TRANSPORT=1 so a caller can say "unreachable" rather than
# "unproven".
_ctc_mark() { [ "$1" = 000 ] && : > "$CTC_TMP/ctc.transport"; return 0; }

_ctc_token_facts() { # <mint-body-file> -> "<token> <exp> <tenant_id>" (any may be empty)
  # The BODY rides the environment, never a path: a native Windows Python
  # cannot open an MSYS /tmp/... path, which read every mint as "no token".
  H_BODY="$(cat "$1" 2>/dev/null)" "${CTC_PY:-python3}" - <<'PY' 2>/dev/null | tr -d '\r' || true
import base64, json, os, sys
tok, e, t = "", "", ""
try:
    d = json.loads(os.environ.get("H_BODY") or "")
    if isinstance(d, dict):
        for k in ("token", "agent_jwt", "jwt", "access_token"):  # envelope-ok: the spellings coord-revive L5 reads
            v = d.get(k)
            if isinstance(v, str) and v:
                tok = v.strip(); break
    seg = tok.split(".")[1]; seg += "=" * (-len(seg) % 4)
    c = json.loads(base64.urlsafe_b64decode(seg))
    e, t = c.get("exp"), c.get("tenant_id")  # envelope-ok: JWT claims, not a fleet response envelope
    e = str(int(e)) if isinstance(e, (int, float)) and not isinstance(e, bool) else ""
    t = t if isinstance(t, str) else ""
except Exception:
    pass
print("%s %s %s" % (tok if " " not in tok else "", e or "", t or ""))
PY
}

_ctc_token_of() { # <mint-body-file> -> the token, or nothing
  H_BODY="$(cat "$1" 2>/dev/null)" "${CTC_PY:-python3}" - <<'PY' 2>/dev/null | tr -d '\r[:space:]' || true
import json, os, sys
try:
    d = json.loads(os.environ.get("H_BODY") or "")
except Exception:
    sys.exit(0)
if isinstance(d, dict):
    for k in ("token", "agent_jwt", "jwt", "access_token"):  # envelope-ok: the spellings coord-revive L5 reads
        v = d.get(k)
        if isinstance(v, str) and v:
            print(v); break
PY
}

# _ctc_runner_port -> $QONTINUI_RUNNER_PORT when it is 1-65535 in digits, else
# 9876 -- the rule coord-credential.psm1 Get-QontinuiRunnerPort applies. Digits
# only: a value like `9876@evil.example` would parse as userinfo + host and
# send the request (and take a token back) from another machine.
_ctc_runner_port() {
  local p="${QONTINUI_RUNNER_PORT:-}"
  p="${p#"${p%%[![:space:]]*}"}"; p="${p%"${p##*[![:space:]]}"}"
  if [[ "$p" =~ ^[0-9]{1,5}$ ]] && (( 10#$p >= 1 && 10#$p <= 65535 )); then
    printf '%s' "$p"
  else
    printf '9876'
  fi
}

# _ctc_runner_mint <tenant> <outfile> -> prints the HTTP code of the local
# runner's UI Bridge mint; the body lands in <outfile> (0600). The tenant is a
# uuid (checked by the caller), so the body needs no JSON escaping. A multi-slot
# runner answers 409 get_coord_device_token:tenant_required to `{}`, so the
# tenant is always sent -- INSIDE `args`: the invoke route's body has one field,
# `args`, and a top-level `tenantId` is silently dropped, which makes the call
# tenant-less (plan 2026-10-05-fleet-scripts-act-for-an-unnamed-tenant-on-a-
# multi-bound-device, F1).
_ctc_runner_mint() {
  local code
  ( umask 077; : > "$2" )
  code="$("${CTC_CURL:-curl}" -sS -o "$(_ctc_np "$2")" -w '%{http_code}' \
    --connect-timeout "${CTC_RUNNER_CONNECT_TIMEOUT:-3}" -m "${CTC_RUNNER_TIMEOUT:-15}" \
    -X POST "http://127.0.0.1:$(_ctc_runner_port)/ui-bridge/invoke/get_coord_device_token" -H 'Content-Type: application/json' \
    -d "{\"args\":{\"tenantId\":\"$1\"}}" 2>/dev/null)" || code=000
  printf '%s' "${code:-000}"
}

# _ctc_runner_token <body-file> -> `OK <token>` when the runner's envelope
# carries a string `data`, else `ERR <why>` (data null for the tenant named =
# the runner holds no credential for it; an `error` code is carried, sanitised
# by the caller).
_ctc_runner_token() {
  H_BODY="$(cat "$1" 2>/dev/null)" "${CTC_PY:-python3}" - <<'PY' 2>/dev/null | tr -d '\r' || true
import json, os
try:
    d = json.loads(os.environ.get("H_BODY") or "")
except Exception:
    print("ERR its answer did not parse"); raise SystemExit(0)
if not isinstance(d, dict):
    print("ERR its answer is not an object"); raise SystemExit(0)
v = d.get("data")  # envelope-ok: the runner UI Bridge invoke envelope {success, data, error}
if isinstance(v, str) and v.strip() and " " not in v.strip():
    print("OK " + v.strip())
elif v is None:
    e = d.get("error")  # envelope-ok: the runner UI Bridge invoke envelope {success, data, error}
    if isinstance(e, dict):
        e = e.get("code") or e.get("message")
    print("ERR " + (str(e) if e else "it holds no credential for this tenant (data: null)"))
else:
    print("ERR its data is not a token")
PY
}

# _ctc_runner_coord_ok -> 0 iff the coord url is production coord. The runner
# mints for the coord it is signed in to, so its token is never sent to a
# local, staging or test coord (it would 401 there and read as UNKNOWN).
_ctc_runner_coord_ok() { local u; u="$(_ctc_url)"; [ "${u,,}" = "https://coord.qontinui.io" ]; }

# _ctc_runner_rung <tenant> <slot> -> `OK <src> <jwt>` or `ERR <why>`. Never
# fatal: every failure is a reason, and the cascade moves on to the mints. A
# usable token is cached in <slot>.runner.jwt and a refusal for that tenant in
# <slot>.runner.rejected. A runner that did not answer at all (connect failure
# or timeout) is recorded once for the whole CTC_TMP in ctc.runner.unreachable,
# so a dead or wedged runner costs one timeout per run, not one per tenant.
_ctc_runner_rung() {
  local want="$1" slot="$2" code r f port fexp fclaim
  if [ -r "$slot.runner.jwt" ]; then
    f="$(tr -d '[:space:]' < "$slot.runner.jwt")"
    _ctc_usable_for "$f" "$want" && { printf 'OK runner(cached) %s' "$f"; return 0; }
  fi
  if [ -r "$slot.runner.rejected" ]; then printf 'ERR %s' "$(cat "$slot.runner.rejected")"; return 0; fi
  if [ -r "$CTC_TMP/ctc.runner.unreachable" ]; then printf 'ERR %s' "$(cat "$CTC_TMP/ctc.runner.unreachable")"; return 0; fi
  port="$(_ctc_runner_port)"
  code="$(_ctc_runner_mint "$want" "$slot.runner.body")"
  if [ "$code" = 200 ]; then
    r="$(_ctc_runner_token "$slot.runner.body")"
  else
    # A non-200 is never a token, whatever its body says; only its error is kept.
    r="ERR $(_ctc_runner_token "$slot.runner.body" | sed 's/^ERR //; s/^OK .*//')"
  fi
  rm -f "$slot.runner.body"
  case "$r" in
    "OK "*)
      f="${r#OK }"
      if _ctc_usable_for "$f" "$want"; then
        ( umask 077; printf '%s\n' "$f" > "$slot.runner.jwt" )
        printf 'OK runner %s' "$f"; return 0
      fi
      # A runner build predating the typed DEVICE_TOKEN_EXPIRED answer hands an
      # EXPIRED token (past the runner's 30 s post-expiry grace, EXPIRY_LEEWAY_SECS:
      # exp <= now - 30, the runner's own jwt_is_expired) for this very
      # tenant back as a success: name that as the
      # runner's expired credential, not as a stale/foreign token.
      fexp="$(_ctc_facts "$f")"; fclaim="${fexp#* }"; fexp="${fexp%% *}"
      if [[ "$fexp" =~ ^[0-9]+$ ]] && (( fexp <= $(date +%s) - 30 )) && [ -n "$fclaim" ] && [ "${fclaim,,}" = "${want,,}" ]; then
        r="RUNNER_MINT_EXPIRED: the runner's coord credential has expired - sign in to the runner (127.0.0.1:$port is an older build that returned the expired token as a success; not used, not retried)"
      else
        r="the local runner's token (127.0.0.1:$port) is stale, claims another tenant, or carries no tenant claim (a legacy token)"
      fi ;;
    *)
      r="${r#ERR }"
      if [ "$code" = 000 ]; then
        r="the local runner (127.0.0.1:$port) did not answer"
        printf '%s' "$r" > "$CTC_TMP/ctc.runner.unreachable"
        printf 'ERR %s' "$r"; return 0
      fi
      case "$r" in
        # The runner's typed 401: its OWN coord credential has expired (plan
        # 2026-10-09-runner-credential-expires-when-coord-is-down-across-its-
        # only-refresh-window). Never "no credential for this tenant": the
        # device is still paired. Cached as a rejection, so the runner is not
        # asked again this run, and the cascade moves on to the mint.
        *"DEVICE_TOKEN_EXPIRED:"*)
          r="RUNNER_MINT_EXPIRED: the runner's coord credential has expired - sign in to the runner (127.0.0.1:$port answered HTTP $code: $(ctc_sanitize "$r"))" ;;
        *) r="the local runner (127.0.0.1:$port) answered HTTP $code${r:+: $(ctc_sanitize "$r")}" ;;
      esac ;;
  esac
  printf '%s' "$r" > "$slot.runner.rejected"
  printf 'ERR %s' "$r"
}

ctc_drop_runner_token() { # <tenant> <why>
  # Nothing can have been cached under an unusable CTC_TMP, so there is nothing
  # to forget: a no-op, returning 0 so a `set -e` caller is not killed by it.
  _ctc_tmp_ok || return 0
  local slot="$CTC_TMP/ctc.mint.${1,,}"
  rm -f "$slot.runner.jwt"
  printf '%s' "$2" > "$slot.runner.rejected"
}

# _ctc_presenter_facts <jwt> -> "<exp> <sub_type> <device_id> <user> <tenant_id>"
# for a token that may be PRESENTED at coord's per-binding door, in ONE
# interpreter start (the hot-path rule _ctc_facts states). A field is the claim
# when it is a plain [A-Za-z0-9._:-] word, `-` when it is absent or empty, `?`
# otherwise; <user> is 1 for a non-empty string user_id, else 0. An ABSENT
# sub_type stays `-`, never `device`: coord's SubType deserializes an absent
# claim as Agent (jwt.rs), and the door answers an agent presenter 403. A reader
# of its own, not a widened _ctc_facts: that one's two-field output is parsed
# outside this file (recon-tenant.sh, install-claude-settings.sh,
# verification-outcome-flush.sh).
_ctc_presenter_facts() {
  H_JWT="$1" "${CTC_PY:-python3}" - <<'PY' 2>/dev/null | tr -d '\r' || true
import base64, json, os, re
plain = re.compile(r"[A-Za-z0-9._:-]{1,128}")
def word(v):
    if v is None or v == "":
        return "-"
    return v if isinstance(v, str) and plain.fullmatch(v) else "?"
try:
    seg = os.environ["H_JWT"].split(".")[1]
    seg += "=" * (-len(seg) % 4)
    pc = json.loads(base64.urlsafe_b64decode(seg))
    if not isinstance(pc, dict):
        raise ValueError("the claims are not an object")
    pe = pc.get("exp")  # envelope-ok: JWT claims, not a fleet response envelope
    pe = str(int(pe)) if isinstance(pe, (int, float)) and not isinstance(pe, bool) else "-"
    pst = pc.get("sub_type")  # envelope-ok: JWT claims; an absent sub_type is Agent at coord, never device
    pu = pc.get("user_id")  # envelope-ok: JWT claims, not a fleet response envelope
    pu = "1" if isinstance(pu, str) and pu.strip() else "0"
    print(" ".join((pe, word(pst), word(pc.get("device_id")), pu, word(pc.get("tenant_id")))))  # envelope-ok: JWT claims, not a fleet response envelope
except Exception:
    print("- - - 0 -")
PY
}

# _ctc_binding_answer <body-file> -> "<token> <exp> <sub_type> <device_id> <tenant_id>"
# for the per-binding door's 200 answer, coord's DeviceRefreshResponse {token,
# device_id, jti, exp, tenant_id} (tokens.rs), in ONE interpreter start. Every
# field after <token> is read off the TOKEN's own claims, never the answer's
# sibling fields: the claims are what every later door checks. Fields as
# _ctc_presenter_facts; <token> is `-` when the answer carries none. The BODY
# rides the environment, never a path (see _ctc_token_facts).
_ctc_binding_answer() {
  H_BODY="$(cat "$1" 2>/dev/null)" "${CTC_PY:-python3}" - <<'PY' 2>/dev/null | tr -d '\r' || true
import base64, json, os, re
plain = re.compile(r"[A-Za-z0-9._:-]{1,128}")
def word(v):
    if v is None or v == "":
        return "-"
    return v if isinstance(v, str) and plain.fullmatch(v) else "?"
tok, rest = "-", ["-", "-", "-", "-"]
try:
    ans = json.loads(os.environ.get("H_BODY") or "")
    t = ans.get("token") if isinstance(ans, dict) else None  # envelope-ok: coord's DeviceRefreshResponse {token, device_id, jti, exp, tenant_id}
    if isinstance(t, str) and t.strip() and " " not in t.strip():
        tok = t.strip()
        seg = tok.split(".")[1]; seg += "=" * (-len(seg) % 4)
        bc = json.loads(base64.urlsafe_b64decode(seg))
        be = bc.get("exp")  # envelope-ok: JWT claims, not a fleet response envelope
        be = str(int(be)) if isinstance(be, (int, float)) and not isinstance(be, bool) else "-"
        rest = [be, word(bc.get("sub_type")), word(bc.get("device_id")), word(bc.get("tenant_id"))]  # envelope-ok: JWT claims, not a fleet response envelope
except Exception:
    pass
print(" ".join([tok] + rest))
PY
}

# _ctc_binding_refusal <body-file> -> a refusal body's `error` string, else its
# `code`, else nothing. ONLY that string ever reaches a reason -- never other
# body text, which could carry a token (the T17a lesson). The caller sanitises.
_ctc_binding_refusal() {
  H_BODY="$(cat "$1" 2>/dev/null)" "${CTC_PY:-python3}" - <<'PY' 2>/dev/null | tr -d '\r' || true
import json, os
try:
    rb = json.loads(os.environ.get("H_BODY") or "")
except Exception:
    raise SystemExit(0)
if isinstance(rb, dict):
    for k in ("error", "code"):
        v = rb.get(k)  # envelope-ok: coord's SeedRefusal {error, code, message} and its flat {"error"} refusals
        if isinstance(v, str) and v.strip():
            print(" ".join(v.split())); break
PY
}

# _ctc_binding_presenter <tenant> -> `OK <device_id> <jwt>` naming the first
# STATIC token, $COORD_DEVICE_JWT then the file, that may be presented at
# coord's per-binding door for <tenant>; else `NONE <why>`, or a bare `NONE`
# when there is no static token at all. A presenter is shaped, more than 60 s
# from expiry, sub_type device, carries a uuid device_id and a user_id, claims a
# uuid tenant OTHER than <tenant>, and -- when this device's id resolves --
# names this device. When no device id resolves, the presenter's own claim
# names the path, which coord checks against the token anyway (the same trust
# the static rungs extend). One presenter: a refusal is never retried with the
# second candidate (the rung below asks at most once).
_ctc_binding_presenter() {
  local want="$1" home_dir="${HOME:-${USERPROFILE:-}}" bt cand cname mydev skipped=""
  local pexp psub pdev puser pten pwhy
  mydev="$(_ctc_device)"
  for cand in env file; do
    bt=""
    if [ "$cand" = env ]; then
      cname='$COORD_DEVICE_JWT'; bt="$(printf '%s' "${COORD_DEVICE_JWT:-}" | tr -d '[:space:]')"
    else
      cname='~/.qontinui/coord-device-jwt'
      if [ -n "$home_dir" ] && [ -r "$home_dir/.qontinui/coord-device-jwt" ]; then
        bt="$(tr -d '[:space:]' < "$home_dir/.qontinui/coord-device-jwt" 2>/dev/null || true)"
      fi
    fi
    [ -n "$bt" ] || continue
    pwhy=""
    if ! _ctc_shaped "$bt"; then pwhy="is not a JWT"
    else
      read -r pexp psub pdev puser pten <<<"$(_ctc_presenter_facts "$bt")"
      if ! [[ "$pexp" =~ ^[0-9]+$ ]] || (( 10#$pexp - $(date +%s) <= 60 )); then pwhy="is expired, exp-less or within 60 s of expiry"
      elif [ "$psub" != device ]; then pwhy="is not a device token (sub_type ${psub/#-/<absent>})"
      elif ! ctc_is_uuid "$pdev"; then pwhy="carries no uuid device_id"
      elif [ "$puser" != 1 ]; then pwhy="carries no user_id"
      elif ! ctc_is_uuid "$pten"; then pwhy="carries no uuid tenant_id"
      elif [ "${pten,,}" = "$want" ]; then pwhy="already claims tenant $want"
      elif [ -n "$mydev" ] && [ "${mydev,,}" != "${pdev,,}" ]; then pwhy="names device $pdev, not this device ($(ctc_sanitize "$mydev"))"
      fi
    fi
    if [ -z "$pwhy" ]; then printf 'OK %s %s' "$pdev" "$bt"; return 0; fi
    skipped="${skipped:+$skipped; }$cname $pwhy"
  done
  printf 'NONE%s' "${skipped:+ $skipped}"
}

# _ctc_binding_verdict <tenant> <device_id> <body-file> -> `OK <jwt>` when the
# door's 200 answer carries a JWT-shaped, more-than-60-s-fresh, sub_type device
# token claiming <tenant> and naming <device_id>; else `ERR <why>`. Every check
# reads the TOKEN's own claims (_ctc_binding_answer, one interpreter start),
# never the answer's sibling fields: the claims are what every later door checks.
_ctc_binding_verdict() {
  local want="$1" pdev="$2" btok bexp bsub bdv bten
  read -r btok bexp bsub bdv bten <<<"$(_ctc_binding_answer "$3")"
  if ! _ctc_shaped "$btok"; then printf 'ERR answered HTTP 200 with no JWT-shaped token -- not used'
  elif ! [[ "$bexp" =~ ^[0-9]+$ ]] || (( 10#$bexp - $(date +%s) <= 60 )); then printf 'ERR returned a token that is expired or exp-less -- not used'
  elif [ "${bten,,}" != "$want" ]; then printf 'ERR returned a token claiming tenant %s -- not used' "$(ctc_sanitize "$bten")"
  elif [ "$bsub" != device ]; then printf 'ERR returned a token that is not a device token (sub_type %s) -- not used' "$(ctc_sanitize "${bsub/#-/<absent>}")"
  elif [ "${bdv,,}" != "${pdev,,}" ]; then printf 'ERR returned a token for device %s, not %s -- not used' "$(ctc_sanitize "$bdv")" "$pdev"
  else printf 'OK %s' "$btok"
  fi
}

# _ctc_binding_rung <tenant> <slot> -> `OK <src> <jwt>`, `ERR <why>`, or `SKIP`
# (nothing to say: no static token to present, or CTC_NO_MINT=1 with nothing
# cached -- the cascade's own `minting is disabled` says that once). Never fatal.
#
# coord's per-binding door, POST /coord/devices/<device_id>/tenant-token
# (tenant_token_seed.rs post_device_tenant_token, qontinui-coord#3173), turns a
# paired device JWT for ONE tenant the device is bound to into a paired device
# JWT for ANOTHER bound tenant: the mint the runner's binding-gap seed uses for
# its per-tenant slots. It writes nothing at coord, and coord's own admission
# rule (tenant_admission::decide_facts) decides whether the binding admits the
# presenter. This rung sits after the runner rung and before the anonymous mint,
# whose bootstrap token coord refuses for every write outside its closeout
# allowlist (auth_bootstrap_scope.rs; finding 267bd8b3): a device whose runner
# is down, signed out or mid-seed still gets a WRITE-capable credential for its
# second tenant, as long as a static presenter is fresh. With none, the cascade
# ends at the bootstrap mint as it did before this rung.
#
# The presenter is _ctc_binding_presenter's; the answer is
# _ctc_binding_verdict's. A usable answer is cached in <slot>.binding.jwt and
# served as binding(cached) while it stays usable (a stale one is re-asked); a
# refusal of ANY kind, 000 included, is cached in <slot>.binding.rejected and is
# never re-asked in the same CTC_TMP, as a refused mint is not retried. "No
# qualifying presenter" sent nothing and is not cached. A 000 is deliberately NOT marked (_ctc_mark):
# CTC_TRANSPORT says "coord unreachable", the mint that follows marks its own
# 000, and this door failing before a WORKING mint is no evidence that coord is
# unreachable. The cost: when coord answers nothing at all, a stage with a
# qualifying presenter waits out two curl timeouts (the door's, then the
# mint's; CTC_CONNECT_TIMEOUT / CTC_TIMEOUT, 10 s / 30 s by default) where it
# waited out one before this rung.
_ctc_binding_rung() {
  local want="$1" slot="$2" bt presenter pdev pjwt bcode verdict refusal
  if [ -r "$slot.binding.jwt" ]; then
    bt="$(tr -d '[:space:]' < "$slot.binding.jwt")"
    _ctc_usable_for "$bt" "$want" && { printf 'OK binding(cached) %s' "$bt"; return 0; }
  fi
  # The door mints (issue_device), so CTC_NO_MINT covers it: no request.
  [ "${CTC_NO_MINT:-0}" = 1 ] && { printf 'SKIP'; return 0; }
  if [ -r "$slot.binding.rejected" ]; then printf 'ERR %s' "$(cat "$slot.binding.rejected")"; return 0; fi
  presenter="$(_ctc_binding_presenter "$want")"
  case "$presenter" in
    "OK "*) presenter="${presenter#OK }"; pdev="${presenter%% *}"; pjwt="${presenter#* }" ;;
    "NONE "*) printf 'ERR the per-binding door was not asked (no qualifying presenter: %s)' "${presenter#NONE }"; return 0 ;;
    *) printf 'SKIP'; return 0 ;;
  esac
  # The presenter rides a 0600 header file, never argv, and is removed right
  # after the call. <want> is a lowercased uuid and <pdev> a uuid (both
  # checked), so the path and the body need no escaping.
  ( umask 077; printf 'Authorization: Bearer %s\n' "$pjwt" > "$slot.binding.hdr"; : > "$slot.binding.body" )
  bcode="$("${CTC_CURL:-curl}" -sS -o "$(_ctc_np "$slot.binding.body")" -w '%{http_code}' --connect-timeout "${CTC_CONNECT_TIMEOUT:-10}" -m "${CTC_TIMEOUT:-30}" \
    -X POST "$(_ctc_url)/coord/devices/$pdev/tenant-token" -H "@$(_ctc_np "$slot.binding.hdr")" -H 'Content-Type: application/json' \
    -d "{\"tenant_id\": \"$want\"}" 2>/dev/null)" || bcode=000
  rm -f "$slot.binding.hdr"
  if [ "$bcode" = 200 ]; then
    verdict="$(_ctc_binding_verdict "$want" "$pdev" "$slot.binding.body")"
  else
    verdict="ERR answered HTTP $(ctc_sanitize "${bcode:-000}")"
    refusal="$(_ctc_binding_refusal "$slot.binding.body")"
    [ -n "$refusal" ] && verdict="$verdict: $(ctc_sanitize "$refusal")"
  fi
  rm -f "$slot.binding.body"
  if [[ "$verdict" == "OK "* ]]; then
    ( umask 077; printf '%s\n' "${verdict#OK }" > "$slot.binding.jwt" )
    printf 'OK binding %s' "${verdict#OK }"; return 0
  fi
  printf 'POST /coord/devices/%s/tenant-token for tenant %s %s' "$pdev" "$want" "${verdict#ERR }" > "$slot.binding.rejected"
  printf 'ERR %s' "$(cat "$slot.binding.rejected")"
}

ctc_stage_for_tenant() {
  local want="${1,,}" home_dir="${HOME:-${USERPROFILE:-}}" jwt="" src="" why="" f code c slot
  _ctc_tmp_ok || { printf 'rejected(%s)' "$_CTC_TMP_WHY"; return 0; }
  rm -f "$CTC_TMP/bearer.hdr"
  ctc_is_uuid "$want" || { printf 'rejected(tenant %s is not a uuid)' "$(ctc_sanitize "${1:-<empty>}")"; return 0; }
  _ctc_url_ok || { printf 'rejected(coord url %s is neither https nor loopback; no credential is sent to it)' "$(_ctc_url)"; return 0; }
  slot="$CTC_TMP/ctc.mint.$want"
  f="$(printf '%s' "${COORD_DEVICE_JWT:-}" | tr -d '[:space:]')"
  if [ -n "$f" ]; then
    if _ctc_usable_for "$f" "$want"; then jwt="$f"; src=env
    else why="\$COORD_DEVICE_JWT is stale or claims another tenant"; fi
  fi
  if [ -z "$jwt" ] && [ -n "$home_dir" ] && [ -r "$home_dir/.qontinui/coord-device-jwt" ]; then
    f="$(tr -d '[:space:]' < "$home_dir/.qontinui/coord-device-jwt" 2>/dev/null || true)"
    if _ctc_usable_for "$f" "$want"; then jwt="$f"; src=file
    else why="${why:+$why; }~/.qontinui/coord-device-jwt is stale or claims another tenant"; fi
  fi
  if [ -z "$jwt" ] && [ "${CTC_NO_RUNNER:-0}" != 1 ] && ! _ctc_runner_coord_ok; then
    why="${why:+$why; }the local runner was not asked (coord url $(_ctc_url) is not https://coord.qontinui.io, the coord its token is minted for)"
  elif [ -z "$jwt" ] && [ "${CTC_NO_RUNNER:-0}" != 1 ]; then
    c="$(_ctc_runner_rung "$want" "$slot")"
    case "$c" in
      "OK "*) c="${c#OK }"; src="${c%% *}"; jwt="${c#* }" ;;
      *) why="${why:+$why; }${c#ERR }" ;;
    esac
  fi
  if [ -z "$jwt" ]; then
    # The per-binding door (see _ctc_binding_rung): before the anonymous mint.
    local brung
    brung="$(_ctc_binding_rung "$want" "$slot")"
    case "$brung" in
      "OK "*) brung="${brung#OK }"; src="${brung%% *}"; jwt="${brung#* }" ;;
      "ERR "*) why="${why:+$why; }${brung#ERR }" ;;
    esac
  fi
  if [ -z "$jwt" ] && [ -r "$slot.jwt" ]; then
    f="$(tr -d '[:space:]' < "$slot.jwt")"
    _ctc_usable_for "$f" "$want" && { jwt="$f"; src="mint(cached)"; }
  fi
  if [ -z "$jwt" ] && [ -r "$slot.rejected" ]; then
    why="${why:+$why; }$(cat "$slot.rejected")"
    # A cached rejection that WAS a transport failure is still one.
    grep -q 'HTTP 000' "$slot.rejected" 2>/dev/null && _ctc_mark 000
  elif [ -z "$jwt" ] && [ "${CTC_NO_MINT:-0}" = 1 ]; then
    why="${why:+$why; }minting is disabled"
  elif [ -z "$jwt" ] && [ "${CTC_NO_ANON_MINT:-0}" = 1 ]; then
    why="${why:+$why; }the anonymous mint is disabled (CTC_NO_ANON_MINT=1)"
  elif [ -z "$jwt" ]; then
    code="$(_ctc_mint "$want" "$slot.body")"
    if [ "$code" = nodev ]; then
      printf 'no device_id ($QONTINUI_MACHINE_ID / ~/.qontinui/machine.json) to mint with' > "$slot.rejected"
    elif [ "$code" != 200 ]; then
      printf 'POST /agents/credential for tenant %s answered HTTP %s' "$want" "${code:-000}" > "$slot.rejected"
    else
      # One interpreter start for the token AND its claims (the hot path).
      c="$(_ctc_token_facts "$slot.body")"; f="${c%% *}"; c="${c#* }"
      if _ctc_shaped "$f" && _ctc_facts_ok "$c" "$want"; then
        jwt="$f"; src=mint
        ( umask 077; printf '%s\n' "$jwt" > "$slot.jwt" )
      else
        # A coord that ignores `tenant_id` mints for the device's legacy
        # pointer. That token is NOT used: every read under it would answer
        # about the wrong tenant.
        c="${c#* }"
        printf 'POST /agents/credential for tenant %s returned a token claiming tenant %s (or one that is expired / exp-less) -- not used' "$want" "$(ctc_sanitize "${c:-<none>}")" > "$slot.rejected"
      fi
    fi
    rm -f "$slot.body"
    [ -n "$jwt" ] || why="${why:+$why; }$(cat "$slot.rejected")"
  fi
  if [ -n "$jwt" ]; then
    ( umask 077; printf 'Authorization: Bearer %s\n' "$jwt" > "$CTC_TMP/bearer.hdr" )
    printf '%s' "$src"
  else
    printf 'rejected(%s)' "${why:-no credential for tenant $want}"
  fi
}

CTC_BINDINGS=""; CTC_BINDINGS_NOTE=""; CTC_BINDINGS_DROPPED=0; _CTC_BINDINGS_READ=0
CTC_BINDING_SLUGS=""; CTC_BINDINGS_COUNT=""; CTC_BINDINGS_MULTI=0
ctc_bindings() {
  local code tok out
  if [ "$_CTC_BINDINGS_READ" = 1 ]; then
    # A read already made stays served (a proven multiplicity is never
    # un-proven); only its transport mark needs a usable CTC_TMP.
    [ "${_CTC_BINDINGS_TRANSPORT:-0}" = 1 ] && _ctc_tmp_ok && _ctc_mark 000
    return 0
  fi
  if ! _ctc_tmp_ok; then
    # A FRESH read is refused: UNKNOWN, and NOT cached, so a later call with a
    # usable CTC_TMP reads afresh.
    CTC_BINDINGS=""; CTC_BINDINGS_DROPPED=0; CTC_BINDING_SLUGS=""; CTC_BINDINGS_COUNT=""; CTC_BINDINGS_MULTI=0
    CTC_BINDINGS_NOTE="$_CTC_TMP_WHY"
    return 0
  fi
  _CTC_BINDINGS_READ=1
  CTC_BINDINGS=""; CTC_BINDINGS_NOTE=""; CTC_BINDINGS_DROPPED=0
  CTC_BINDING_SLUGS=""; CTC_BINDINGS_COUNT=""; CTC_BINDINGS_MULTI=0
  if ! _ctc_url_ok; then CTC_BINDINGS_NOTE="coord url $(_ctc_url) is neither https nor loopback"; return 0; fi
  if [ "${CTC_NO_MINT:-0}" = 1 ]; then CTC_BINDINGS_NOTE="minting is disabled, so the binding list was not read"; return 0; fi
  if [ "${CTC_NO_ANON_MINT:-0}" = 1 ]; then CTC_BINDINGS_NOTE="the anonymous mint is disabled (CTC_NO_ANON_MINT=1), so the binding list was not read"; return 0; fi
  code="$(_ctc_mint "" "$CTC_TMP/ctc.anon.body")"
  case "$code" in
    nodev) CTC_BINDINGS_NOTE="no device_id to mint with"; rm -f "$CTC_TMP/ctc.anon.body"; return 0 ;;
    200) ;;
    422)
      # coord in `live` resolution mode refuses a tenant-less mint for a device
      # bound to several tenants (agent_worktrees::tenant_ambiguous_body,
      # reason multi_bound_no_tenant). That refusal is PROOF of >= 2 bindings
      # even though it names none: it carries a count, never the ids. Read in
      # bash -- the body is coord's own small JSON object.
      local _b re_err='"error"[[:space:]]*:[[:space:]]*"tenant_ambiguous"' re_n='"candidate_count"[[:space:]]*:[[:space:]]*([0-9]+)'
      _b="$(cat "$CTC_TMP/ctc.anon.body" 2>/dev/null)"; rm -f "$CTC_TMP/ctc.anon.body"
      if [[ "$_b" =~ $re_err ]]; then
        CTC_BINDINGS_MULTI=1
        [[ "$_b" =~ $re_n ]] && CTC_BINDINGS_COUNT="${BASH_REMATCH[1]}"
      fi
      # The note keeps the spelling every other non-200 arm uses (callers'
      # suites match on it); the multiplicity rides CTC_BINDINGS_MULTI /
      # CTC_BINDINGS_COUNT, and _ctc_tenant_required_line says what it means.
      CTC_BINDINGS_NOTE="the tenant-less POST /agents/credential answered HTTP 422"
      return 0 ;;
    *) [ "${code:-000}" = 000 ] && _CTC_BINDINGS_TRANSPORT=1
       CTC_BINDINGS_NOTE="the tenant-less POST /agents/credential answered HTTP ${code:-000}"; rm -f "$CTC_TMP/ctc.anon.body"; return 0 ;;
  esac
  tok="$(_ctc_token_of "$CTC_TMP/ctc.anon.body")"; rm -f "$CTC_TMP/ctc.anon.body"
  _ctc_shaped "$tok" || { CTC_BINDINGS_NOTE="the tenant-less mint returned no token"; return 0; }
  ( umask 077; printf 'Authorization: Bearer %s\n' "$tok" > "$CTC_TMP/ctc.anon.hdr" )
  _ctc_identity_read "$CTC_TMP/ctc.anon.hdr"
  rm -f "$CTC_TMP/ctc.anon.hdr"
  return 0
}

# _ctc_identity_read <bearer-header-file> -- POST /mcp coord_query_identity
# under that bearer and fill CTC_BINDINGS / CTC_BINDINGS_DROPPED /
# CTC_BINDING_SLUGS / CTC_BINDINGS_COUNT / CTC_BINDINGS_MULTI, or set
# CTC_BINDINGS_NOTE. The caller resets those first. The binding list is the
# DEVICE's, whichever of its tenants the bearer acts for.
_ctc_identity_read() {
  local code out
  : > "$CTC_TMP/ctc.ident.json"
  code="$("${CTC_CURL:-curl}" -sS -o "$(_ctc_np "$CTC_TMP/ctc.ident.json")" -w '%{http_code}' --connect-timeout "${CTC_CONNECT_TIMEOUT:-10}" -m "${CTC_TIMEOUT:-30}" \
    -X POST "$(_ctc_url)/mcp" -H "@$(_ctc_np "$1")" -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"coord_query_identity","arguments":{}}}' 2>/dev/null)" || code=000
  _ctc_mark "${code:-000}"
  [ "${code:-000}" = 000 ] && _CTC_BINDINGS_TRANSPORT=1
  if [ "$code" != 200 ]; then CTC_BINDINGS_NOTE="POST /mcp coord_query_identity answered HTTP ${code:-000}"; return 0; fi
  # The body goes to Python on STDIN, never through the environment: an
  # answer over ~128 KiB would fail exec with E2BIG and read as "did not
  # parse". So the program itself is passed with -c.
  out="$("${CTC_PY:-python3}" -c "$(cat <<'PY'
import json, os, sys
try:
    d = json.loads(sys.stdin.read() or "")
    r = d["result"]  # envelope-ok: JSON-RPC result of POST /mcp tools/call
    s = r.get("structuredContent")
    if not isinstance(s, dict):
        s = json.loads(r["content"][0]["text"])
    b = s["device_tenant_bindings"]["tenant_ids"]
except Exception:
    print("ERR the identity answer did not parse"); sys.exit(0)
if b is None:
    print("ERR device_tenant_bindings.tenant_ids is null (coord could not read the bindings)"); sys.exit(0)
if not isinstance(b, list):
    print("ERR device_tenant_bindings.tenant_ids is not a list"); sys.exit(0)
# Validated HERE, in one place: one uuid per line, and a count of EVERY other
# entry (a non-object, a null / non-string / empty tenant_id, a string that is
# not a uuid -- including one with an embedded newline), so nothing served can
# vanish before it is counted and nothing served is ever word-split by a shell.
import re
uuid_re = re.compile(r"[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}")
# A slug is kept only when it is a plain word: it is printed into messages and
# split on whitespace by the shell, so anything else is dropped (the binding
# itself still counts) and that tenant reads `slug UNKNOWN`.
slug_re = re.compile(r"[A-Za-z0-9._-]{1,64}")
ids, dropped = [], 0
for x in b:
    t = x.get("tenant_id") if isinstance(x, dict) else None
    if isinstance(t, str) and uuid_re.fullmatch(t):
        g = x.get("tenant_slug")
        ids.append((t, g if isinstance(g, str) and slug_re.fullmatch(g) else "-"))
    else:
        dropped += 1
print("OK %d" % dropped)
for t, g in ids:
    print("%s %s" % (t, g))
PY
)" <"$CTC_TMP/ctc.ident.json" 2>/dev/null | tr -d '\r')"
  case "$out" in
    "OK "[0-9]*)
      # Only uuids are bindings; the Python above already validated them, one
      # per line, and COUNTED everything else (CTC_BINDINGS_DROPPED), so a
      # caller that needs the complete list can refuse rather than act on a
      # silently shortened one. `mapfile`, never word splitting: nothing
      # served is globbed against the caller's cwd.
      local _w _ws _id _sl _n=0
      mapfile -t _ws <<<"$out"
      CTC_BINDINGS_DROPPED="${_ws[0]#OK }"
      for _w in "${_ws[@]:1}"; do
        [ -n "$_w" ] || continue
        _id="${_w%% *}"; _sl="${_w#* }"
        CTC_BINDINGS="${CTC_BINDINGS:+$CTC_BINDINGS }$_id"; _n=$((_n + 1))
        [ "$_sl" != "-" ] && [ "$_sl" != "$_w" ] && CTC_BINDING_SLUGS="${CTC_BINDING_SLUGS:+$CTC_BINDING_SLUGS }${_id,,}=$_sl"
      done
      # Dropped entries are still bindings coord SERVED: they count toward the
      # multiplicity, so one uuid beside one malformed entry is NOT "single".
      CTC_BINDINGS_COUNT=$((_n + CTC_BINDINGS_DROPPED))
      [ "$CTC_BINDINGS_COUNT" -ge 2 ] && CTC_BINDINGS_MULTI=1
      [ "$CTC_BINDINGS_DROPPED" = 0 ] || CTC_BINDINGS_NOTE="dropped $CTC_BINDINGS_DROPPED served binding(s) that are not uuids"
      [ -n "$CTC_BINDINGS" ] || CTC_BINDINGS_NOTE="${CTC_BINDINGS_NOTE:-this device is bound to no tenant}" ;;
    *) CTC_BINDINGS_NOTE="${out#ERR }"; [ -n "$CTC_BINDINGS_NOTE" ] || CTC_BINDINGS_NOTE="the identity answer did not parse" ;;
  esac
  return 0
}

# ctc_bindings_over <bearer-header-file> -- read the binding list and its slugs
# over a credential the caller ALREADY holds (a tenant-scoped one). On a device
# whose coord runs `live` resolution the tenant-less mint ctc_bindings needs is
# refused (422 tenant_ambiguous), so its list -- and every slug -- stays
# UNKNOWN; a tenant-scoped bearer reads the same list. Fills the CTC_BINDINGS*
# fields ONLY on a readable answer; on any failure they are left exactly as
# they were (UNKNOWN stays UNKNOWN). Call directly, never in $(...).
ctc_bindings_over() {
  local sv_b="$CTC_BINDINGS" sv_n="$CTC_BINDINGS_NOTE" sv_d="$CTC_BINDINGS_DROPPED" sv_s="$CTC_BINDING_SLUGS" \
        sv_c="$CTC_BINDINGS_COUNT" sv_m="$CTC_BINDINGS_MULTI" sv_t="${_CTC_BINDINGS_TRANSPORT:-0}"
  [ -s "${1:-}" ] || return 0
  _ctc_tmp_ok || return 0
  _ctc_url_ok || return 0
  CTC_BINDINGS=""; CTC_BINDINGS_NOTE=""; CTC_BINDINGS_DROPPED=0; CTC_BINDING_SLUGS=""; CTC_BINDINGS_COUNT=""; CTC_BINDINGS_MULTI=0
  _ctc_identity_read "$1"
  _CTC_BINDINGS_TRANSPORT="$sv_t"
  if [ -n "$CTC_BINDINGS_COUNT" ] && [ -n "$CTC_BINDINGS" ]; then
    _CTC_BINDINGS_READ=1
    # A proven multiplicity is never un-proven by a later read.
    [ "$sv_m" = 1 ] && CTC_BINDINGS_MULTI=1
  else
    CTC_BINDINGS="$sv_b"; CTC_BINDINGS_NOTE="$sv_n"; CTC_BINDINGS_DROPPED="$sv_d"
    CTC_BINDING_SLUGS="$sv_s"; CTC_BINDINGS_COUNT="$sv_c"; CTC_BINDINGS_MULTI="$sv_m"
  fi
  return 0
}

# _ctc_probe <owner/repo> -> the HTTP code of the ownership door under the
# staged bearer; a 200 whose body names another repo is reported as `bad`.
_ctc_probe() {
  local enc="${1//\//%2F}" code
  : > "$CTC_TMP/ctc.probe.json"
  code="$("${CTC_CURL:-curl}" -sS -o "$(_ctc_np "$CTC_TMP/ctc.probe.json")" -w '%{http_code}' --connect-timeout "${CTC_CONNECT_TIMEOUT:-10}" -m "${CTC_TIMEOUT:-30}" \
    -H "@$(_ctc_np "$CTC_TMP/bearer.hdr")" "$(_ctc_url)/pr-merge/$enc/0/author-session" 2>/dev/null)" || code=000
  _ctc_mark "${code:-000}"
  if [ "$code" = 200 ]; then
    # The 200 must be ABOUT this repo. Read in bash, not an interpreter: this
    # runs once per repo per candidate, on every caller's hot path. A slug has
    # no quote or backslash in it, so the first "repo" string value is exact.
    local body re='"repo"[[:space:]]*:[[:space:]]*"([^"]*)"'
    body="$(cat "$CTC_TMP/ctc.probe.json" 2>/dev/null)"
    if ! [[ "$body" =~ $re ]] || [ "${BASH_REMATCH[1]}" != "$1" ]; then code=bad; fi
  fi
  printf '%s' "${code:-000}"
}

CTC_STATE=""; CTC_TENANT=""; CTC_NOTE=""; CTC_BEARER_SRC=""; CTC_TRANSPORT=0
ctc_owner_tenant() {
  if ! _ctc_tmp_ok; then
    CTC_STATE=unknown; CTC_TENANT=""; CTC_BEARER_SRC=""; CTC_TRANSPORT=0
    CTC_NOTE="$_CTC_TMP_WHY, so which tenant owns ${*:-<no repo named>} is UNKNOWN"
    return 0
  fi
  rm -f "$CTC_TMP/ctc.transport"
  _ctc_owner_walk "$@"
  CTC_TRANSPORT=0
  [ "$CTC_STATE" != proven ] && [ -e "$CTC_TMP/ctc.transport" ] && CTC_TRANSPORT=1
  return 0
}
_ctc_owner_walk() {
  local -a cands=() srcs=() repos=("$@")
  local seen=" " t s i=0 src code r note="" refuted=0 untried=0 active home_dir="${HOME:-${USERPROFILE:-}}" bindings_done=0
  CTC_STATE=unknown; CTC_TENANT=""; CTC_NOTE=""; CTC_BEARER_SRC=""
  rm -f "$CTC_TMP/bearer.hdr"
  if [ "${#repos[@]}" -eq 0 ]; then CTC_NOTE="no repo named"; return 0; fi
  for r in "${repos[@]}"; do
    case "$r" in ?*/?*) ;; *) CTC_NOTE="'$r' is not an owner/name slug, so no tenant can be proven to own it"; return 0 ;; esac
  done
  _ctc_add() { # <tenant> <source>
    local x="${1,,}"
    case "$seen" in *" $x "*) return 0 ;; esac
    seen="$seen$x "; cands+=("$x"); srcs+=("$2")
  }
  if [ -n "${QONTINUI_TENANT_ID:-}" ]; then
    if ctc_is_uuid "$QONTINUI_TENANT_ID"; then _ctc_add "$QONTINUI_TENANT_ID" "\$QONTINUI_TENANT_ID"
    else note="${note}\$QONTINUI_TENANT_ID is not a uuid (ignored); "; fi
  fi
  if [ -n "$home_dir" ] && [ -r "$home_dir/.qontinui/machine.json" ]; then
    active="$(_ctc_machine_json_field "$home_dir/.qontinui/machine.json" active_tenant_id)"
    ctc_is_uuid "$active" && _ctc_add "$active" "machine.json active_tenant_id"
  fi
  while :; do
    if [ "$i" -ge "${#cands[@]}" ]; then
      [ "$bindings_done" = 1 ] && break
      bindings_done=1
      ctc_bindings
      if [ -n "$CTC_BINDINGS" ]; then
        # ctc_bindings already kept only uuids; a served binding it DROPPED
        # could have been the owner, so it counts as untried (the walk then
        # ends unknown, never refuted) and its note is carried.
        local _bs
        read -r -a _bs <<<"$CTC_BINDINGS"
        for t in "${_bs[@]}"; do _ctc_add "$t" "device_tenant_bindings"; done
        if [ "${CTC_BINDINGS_DROPPED:-0}" != 0 ]; then
          note="${note}${CTC_BINDINGS_NOTE}; "
          untried=$((untried + 1))
        fi
      else
        note="${note}the device's tenant bindings are UNKNOWN ($CTC_BINDINGS_NOTE); "
        untried=$((untried + 1))
      fi
      continue
    fi
    t="${cands[$i]}"; s="${srcs[$i]}"; i=$((i + 1))
    src="$(ctc_stage_for_tenant "$t")"
    case "$src" in
      rejected*) note="${note}$t (from $s): no credential for it, ${src#rejected}; "; untried=$((untried + 1)); continue ;;
    esac
    code=200
    for r in "${repos[@]}"; do
      code="$(_ctc_probe "$r")"
      [ "$code" = 200 ] || break
    done
    if [ "$code" = 401 ] && [[ "$src" == runner* ]]; then
      # Coord refused the runner's token: forget it and re-stage ONCE, which
      # now reaches the mint rungs.
      ctc_drop_runner_token "$t" "coord answered 401 to the local runner's token for tenant $t"
      note="${note}$t (from $s): coord answered 401 to the runner's token, re-staged; "
      src="$(ctc_stage_for_tenant "$t")"
      case "$src" in
        rejected*) note="${note}$t (from $s): no credential for it, ${src#rejected}; "; untried=$((untried + 1)); continue ;;
      esac
      src="runner(401)→$src"
      code=200
      for r in "${repos[@]}"; do
        code="$(_ctc_probe "$r")"
        [ "$code" = 200 ] || break
      done
    fi
    case "$code" in
      200)
        CTC_STATE=proven; CTC_TENANT="$t"; CTC_BEARER_SRC="$src"
        CTC_NOTE="${note}proven: tenant $t owns ${repos[*]} (author-session door answered 200 under a token claiming it; candidate from $s, bearer=$src)"
        return 0 ;;
      404)
        refuted=$((refuted + 1)); rm -f "$CTC_TMP/bearer.hdr"
        note="${note}$t (from $s): 404 for $r, not this tenant's; " ;;
      *)
        rm -f "$CTC_TMP/bearer.hdr"
        CTC_NOTE="${note}$t (from $s): the ownership door answered ${code} for $r -- the door failed, so which tenant owns ${repos[*]} is UNKNOWN"
        return 0 ;;
    esac
  done
  rm -f "$CTC_TMP/bearer.hdr"
  if [ "$refuted" -gt 0 ] && [ "$untried" -eq 0 ]; then
    CTC_STATE=refuted
    CTC_NOTE="${note}no tenant this device is bound to owns ${repos[*]} -- the owning tenant is UNKNOWN"
  else
    CTC_NOTE="${note}no candidate tenant was proven to own ${repos[*]}$( [ "$untried" -gt 0 ] && printf ' (%s candidate(s) or the binding list could not be tried)' "$untried") -- the owning tenant is UNKNOWN"
  fi
  return 0
}

CTC_DOOR_CODE=""
ctc_prove_tenant() { # <tenant> <owner/repo>
  local t="${1,,}" r="$2" src
  CTC_STATE=unknown; CTC_TENANT=""; CTC_NOTE=""; CTC_BEARER_SRC=""; CTC_DOOR_CODE=""
  _ctc_tmp_ok || { CTC_NOTE="$_CTC_TMP_WHY, so whether tenant $t owns $r is UNKNOWN"; return 0; }
  case "$r" in ?*/?*) ;; *) CTC_NOTE="'$r' is not an owner/name slug"; return 0 ;; esac
  src="$(ctc_stage_for_tenant "$t")"
  case "$src" in
    rejected*) CTC_BEARER_SRC="$src"; CTC_NOTE="no credential for tenant $t ${src#rejected}"; return 0 ;;
  esac
  CTC_BEARER_SRC="$src"
  CTC_DOOR_CODE="$(_ctc_probe "$r")"
  if [ "$CTC_DOOR_CODE" = 401 ] && [[ "$src" == runner* ]]; then
    # Coord refused the runner's token: forget it and re-stage ONCE (mint rungs).
    ctc_drop_runner_token "$t" "coord answered 401 to the local runner's token for tenant $t"
    src="$(ctc_stage_for_tenant "$t")"
    case "$src" in
      rejected*) rm -f "$CTC_TMP/bearer.hdr"; CTC_BEARER_SRC="runner(401)→$src"; CTC_NOTE="coord answered 401 to the runner's token for tenant $t and no other credential could be staged ${src#rejected}"; return 0 ;;
    esac
    CTC_BEARER_SRC="runner(401)→$src"; src="$CTC_BEARER_SRC"
    CTC_DOOR_CODE="$(_ctc_probe "$r")"
  fi
  rm -f "$CTC_TMP/bearer.hdr"
  case "$CTC_DOOR_CODE" in
    200) CTC_STATE=proven; CTC_TENANT="$t"; CTC_NOTE="proven: tenant $t owns $r (author-session door answered 200, bearer=$src)" ;;
    404) CTC_STATE=refuted; CTC_NOTE="tenant $t does not own $r (author-session door answered 404 under a token claiming it, bearer=$src)" ;;
    *)   CTC_NOTE="the author-session door answered HTTP $CTC_DOOR_CODE for $r (bearer=$src) -- which tenant owns it is UNKNOWN" ;;
  esac
  return 0
}

ctc_slug_of() { # <tenant> -> the slug ctc_bindings read for it, or nothing (pure)
  local want="${1,,}" p
  [ -n "$want" ] || return 0
  for p in $CTC_BINDING_SLUGS; do
    [ "${p%%=*}" = "$want" ] && { printf '%s' "${p#*=}"; return 0; }
  done
  return 0
}

ctc_tenant_label() { # <tenant> -> `<slug> (<uuid>)` or `slug UNKNOWN (<uuid>)` (pure)
  local g
  g="$(ctc_slug_of "$1")"
  printf '%s (%s)' "${g:-slug UNKNOWN}" "$(ctc_sanitize "$1")"
}

# The typed refusal for a device PROVEN multi-bound with no tenant named. One
# spelling, here, so every caller prints the same line.
_ctc_tenant_required_line() {
  local names="" t g
  if [ -n "$CTC_BINDINGS" ]; then
    for t in $CTC_BINDINGS; do
      g="$(ctc_slug_of "$t")"
      names="${names:+$names, }${g:-slug UNKNOWN} $t"
    done
    [ "${CTC_BINDINGS_DROPPED:-0}" = 0 ] || names="$names, and $CTC_BINDINGS_DROPPED served binding(s) that are not uuids"
  else
    names="slugs UNKNOWN: coord's tenant_ambiguous answer carries a count, not the tenants"
  fi
  printf 'TENANT_REQUIRED: this device is bound to %s tenants (%s); set QONTINUI_TENANT_ID or pass --tenant' \
    "${CTC_BINDINGS_COUNT:-several}" "$names"
}

CTC_EXPECTED_STATE=""; CTC_EXPECTED_TENANT=""; CTC_EXPECTED_SOURCE=""; CTC_EXPECTED_SLUG=""; CTC_EXPECTED_NOTE=""
ctc_expected_tenant() { # [--tenant <uuid>] [--owner-of <owner/repo>] [--no-slug]
  local flag="" flag_set=0 owner="" want_slug=1 note=""
  CTC_EXPECTED_STATE=""; CTC_EXPECTED_TENANT=""; CTC_EXPECTED_SOURCE=""; CTC_EXPECTED_SLUG=""; CTC_EXPECTED_NOTE=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --tenant)   flag="${2:-}"; flag_set=1; shift 2 || shift ;;
      --owner-of) owner="${2:-}"; shift 2 || shift ;;
      --no-slug)  want_slug=0; shift ;;
      *) CTC_EXPECTED_STATE=invalid
         CTC_EXPECTED_NOTE="ctc_expected_tenant: unknown argument $(ctc_sanitize "$1")"; return 0 ;;
    esac
  done
  # 1. the explicit flag; 2. $QONTINUI_TENANT_ID. A malformed value is an
  # ERROR: ignoring it would act for whatever the later steps find, which is
  # not the tenant the caller named.
  if [ "$flag_set" = 1 ]; then
    if ctc_is_uuid "$flag"; then CTC_EXPECTED_TENANT="${flag,,}"; CTC_EXPECTED_SOURCE="--tenant"
    else CTC_EXPECTED_STATE=invalid; CTC_EXPECTED_NOTE="--tenant $(ctc_sanitize "${flag:-<empty>}") is not a tenant uuid"; return 0; fi
  elif [ -n "${QONTINUI_TENANT_ID:-}" ]; then
    if ctc_is_uuid "$QONTINUI_TENANT_ID"; then CTC_EXPECTED_TENANT="${QONTINUI_TENANT_ID,,}"; CTC_EXPECTED_SOURCE="\$QONTINUI_TENANT_ID"
    else CTC_EXPECTED_STATE=invalid; CTC_EXPECTED_NOTE="\$QONTINUI_TENANT_ID=$(ctc_sanitize "$QONTINUI_TENANT_ID") is not a tenant uuid"; return 0; fi
  fi
  # Steps 1-2 write nothing, so a malformed argument above is still `invalid`.
  # Past them, even a --tenant answer is refused: the slug read, the owner
  # proof and the binding read all write under $CTC_TMP, so a caller in breach
  # of the contract hears it here rather than from whichever step it reaches.
  _ctc_tmp_ok || { CTC_EXPECTED_STATE=unknown; CTC_EXPECTED_TENANT=""; CTC_EXPECTED_SOURCE=""; CTC_EXPECTED_NOTE="$_CTC_TMP_WHY, so the tenant to act for is UNKNOWN"; return 0; }
  # 3. the tenant PROVEN to own a named repo (a workspace-global render names
  # the repo it renders from; a per-session door names none).
  if [ -z "$CTC_EXPECTED_TENANT" ] && [ -n "$owner" ]; then
    ctc_owner_tenant "$owner"
    # The proof left the owner's tenant-scoped bearer staged: read the slugs
    # over it when the tenant-less read could not (422 on a live-mode coord).
    [ "$CTC_STATE" = proven ] && [ -z "$CTC_BINDING_SLUGS" ] && ctc_bindings_over "$CTC_TMP/bearer.hdr"
    rm -f "$CTC_TMP/bearer.hdr"
    if [ "$CTC_STATE" = proven ]; then
      CTC_EXPECTED_TENANT="${CTC_TENANT,,}"; CTC_EXPECTED_SOURCE="proven owner of $owner"
    else
      note="owner of $owner not proven ($CTC_STATE): $CTC_NOTE; "
    fi
  fi
  # 4. the device's SOLE binding -- only when the list is read, complete, and one.
  if [ -z "$CTC_EXPECTED_TENANT" ]; then
    ctc_bindings
    if [ "${CTC_BINDINGS_MULTI:-0}" = 1 ]; then
      CTC_EXPECTED_STATE=required; CTC_EXPECTED_NOTE="${note}$(_ctc_tenant_required_line)"; return 0
    fi
    if [ -n "$CTC_BINDINGS" ] && [ "${CTC_BINDINGS_DROPPED:-0}" = 0 ] && [ "${CTC_BINDINGS% *}" = "$CTC_BINDINGS" ]; then
      CTC_EXPECTED_TENANT="${CTC_BINDINGS,,}"; CTC_EXPECTED_SOURCE="the device's sole binding"
    else
      CTC_EXPECTED_STATE=unknown
      CTC_EXPECTED_NOTE="${note}no tenant named and the device's bindings are UNKNOWN (${CTC_BINDINGS_NOTE:-no binding list}), so whether this device needs one is UNKNOWN; set QONTINUI_TENANT_ID or pass --tenant"
      return 0
    fi
  fi
  CTC_EXPECTED_STATE=resolved; CTC_EXPECTED_NOTE="$note"
  if [ "$want_slug" = 1 ]; then
    ctc_bindings
    CTC_EXPECTED_SLUG="$(ctc_slug_of "$CTC_EXPECTED_TENANT")"
  fi
  return 0
}

ctc_origin_slug() { # <checkout-dir>
  local url
  url="$(git --no-optional-locks -C "$(_ctc_np "$1")" config --get remote.origin.url 2>/dev/null | tr -d '\r')"
  case "$url" in
    https://github.com/*|http://github.com/*|ssh://git@github.com/*|git@github.com:*) ;;
    *) return 0 ;;
  esac
  url="${url#*github.com}"; url="${url#[:/]}"; url="${url%/}"; url="${url%.git}"
  case "$url" in
    */*/*|*[!A-Za-z0-9._/-]*) return 0 ;;
    ?*/?*) printf '%s' "$url" ;;
  esac
}
