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
#   CTC_TMP        a private (0700) directory this file may write into.
#   CTC_COORD_URL  coord base (default $COORD_HTTP_URL, else https://coord.qontinui.io).
#   CTC_CURL       curl binary or a test stub (default curl).
#   CTC_PY         a Python 3 (default python3).
#   CTC_TIMEOUT    seconds per HTTP call (default 30).
#   CTC_DEVICE     device id (default $QONTINUI_MACHINE_ID, else
#                  ~/.qontinui/machine.json device_id / machine_id).
#   CTC_NO_MINT=1  never mint; only a static token claiming the tenant is used.
# Bearers are staged in 0600 header files and passed as `curl -H @file`, never
# on argv, and are only ever sent to an https or loopback coord.
#
# FUNCTIONS
#   ctc_stage_for_tenant <tenant>  -> writes $CTC_TMP/bearer.hdr; prints its
#       source (env|file|mint|mint(cached)) or `rejected(<why>)` and writes
#       nothing. Only a token whose `tenant_id` claim IS <tenant> is staged.
#   ctc_bindings                   -> CTC_BINDINGS (space-separated tenant ids)
#       or empty with CTC_BINDINGS_NOTE. Call directly, never in $(...).
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

ctc_is_uuid() {
  local re='^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$'
  [[ "${1:-}" =~ $re ]]
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
  code="$("${CTC_CURL:-curl}" -sS -o "$(_ctc_np "$2")" -w '%{http_code}' --connect-timeout 10 -m "${CTC_TIMEOUT:-30}" \
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
  "${CTC_PY:-python3}" - "$1" <<'PY' 2>/dev/null | tr -d '\r' || true
import base64, json, sys
tok, e, t = "", "", ""
try:
    d = json.load(open(sys.argv[1]))
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
  "${CTC_PY:-python3}" - "$1" <<'PY' 2>/dev/null | tr -d '\r[:space:]' || true
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    sys.exit(0)
if isinstance(d, dict):
    for k in ("token", "agent_jwt", "jwt", "access_token"):  # envelope-ok: the spellings coord-revive L5 reads
        v = d.get(k)
        if isinstance(v, str) and v:
            print(v); break
PY
}

ctc_stage_for_tenant() {
  local want="${1,,}" home_dir="${HOME:-${USERPROFILE:-}}" jwt="" src="" why="" f code c slot
  rm -f "$CTC_TMP/bearer.hdr"
  ctc_is_uuid "$want" || { printf 'rejected(tenant %s is not a uuid)' "${1:-<empty>}"; return 0; }
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
        printf 'POST /agents/credential for tenant %s returned a token claiming tenant %s (or one that is expired / exp-less) -- not used' "$want" "${c:-<none>}" > "$slot.rejected"
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

CTC_BINDINGS=""; CTC_BINDINGS_NOTE=""; _CTC_BINDINGS_READ=0
ctc_bindings() {
  local code tok out
  if [ "$_CTC_BINDINGS_READ" = 1 ]; then
    [ "${_CTC_BINDINGS_TRANSPORT:-0}" = 1 ] && _ctc_mark 000
    return 0
  fi
  _CTC_BINDINGS_READ=1
  CTC_BINDINGS=""; CTC_BINDINGS_NOTE=""
  if ! _ctc_url_ok; then CTC_BINDINGS_NOTE="coord url $(_ctc_url) is neither https nor loopback"; return 0; fi
  if [ "${CTC_NO_MINT:-0}" = 1 ]; then CTC_BINDINGS_NOTE="minting is disabled, so the binding list was not read"; return 0; fi
  code="$(_ctc_mint "" "$CTC_TMP/ctc.anon.body")"
  case "$code" in
    nodev) CTC_BINDINGS_NOTE="no device_id to mint with"; rm -f "$CTC_TMP/ctc.anon.body"; return 0 ;;
    200) ;;
    *) [ "${code:-000}" = 000 ] && _CTC_BINDINGS_TRANSPORT=1
       CTC_BINDINGS_NOTE="the tenant-less POST /agents/credential answered HTTP ${code:-000}"; rm -f "$CTC_TMP/ctc.anon.body"; return 0 ;;
  esac
  tok="$(_ctc_token_of "$CTC_TMP/ctc.anon.body")"; rm -f "$CTC_TMP/ctc.anon.body"
  _ctc_shaped "$tok" || { CTC_BINDINGS_NOTE="the tenant-less mint returned no token"; return 0; }
  ( umask 077; printf 'Authorization: Bearer %s\n' "$tok" > "$CTC_TMP/ctc.anon.hdr" )
  : > "$CTC_TMP/ctc.ident.json"
  code="$("${CTC_CURL:-curl}" -sS -o "$(_ctc_np "$CTC_TMP/ctc.ident.json")" -w '%{http_code}' --connect-timeout 10 -m "${CTC_TIMEOUT:-30}" \
    -X POST "$(_ctc_url)/mcp" -H "@$(_ctc_np "$CTC_TMP/ctc.anon.hdr")" -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"coord_query_identity","arguments":{}}}' 2>/dev/null)" || code=000
  rm -f "$CTC_TMP/ctc.anon.hdr"
  _ctc_mark "${code:-000}"
  [ "${code:-000}" = 000 ] && _CTC_BINDINGS_TRANSPORT=1
  if [ "$code" != 200 ]; then CTC_BINDINGS_NOTE="POST /mcp coord_query_identity answered HTTP ${code:-000}"; return 0; fi
  out="$("${CTC_PY:-python3}" - "$CTC_TMP/ctc.ident.json" <<'PY' 2>/dev/null | tr -d '\r'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
    r = d["result"]  # envelope-ok: JSON-RPC result of POST /mcp tools/call
    s = r.get("structuredContent")
    if not isinstance(s, dict):
        s = json.loads(r["content"][0]["text"])
    b = s["device_tenant_bindings"]["tenant_ids"]
except Exception:
    print("ERR the identity answer did not parse"); sys.exit(0)
if b is None:
    print("ERR device_tenant_bindings.tenant_ids is null (coord could not read the bindings)"); sys.exit(0)
ids = [x.get("tenant_id") for x in b if isinstance(x, dict) and isinstance(x.get("tenant_id"), str)]
print("OK " + " ".join(ids))
PY
)"
  case "$out" in
    "OK "?*) CTC_BINDINGS="${out#OK }" ;;
    "OK "|OK) CTC_BINDINGS_NOTE="this device is bound to no tenant" ;;
    *) CTC_BINDINGS_NOTE="${out#ERR }"; [ -n "$CTC_BINDINGS_NOTE" ] || CTC_BINDINGS_NOTE="the identity answer did not parse" ;;
  esac
  return 0
}

# _ctc_probe <owner/repo> -> the HTTP code of the ownership door under the
# staged bearer; a 200 whose body names another repo is reported as `bad`.
_ctc_probe() {
  local enc="${1//\//%2F}" code
  : > "$CTC_TMP/ctc.probe.json"
  code="$("${CTC_CURL:-curl}" -sS -o "$(_ctc_np "$CTC_TMP/ctc.probe.json")" -w '%{http_code}' --connect-timeout 10 -m "${CTC_TIMEOUT:-30}" \
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
        for t in $CTC_BINDINGS; do
          if ctc_is_uuid "$t"; then _ctc_add "$t" "device_tenant_bindings"; else note="${note}binding '$t' is not a uuid (ignored); "; fi
        done
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
  case "$r" in ?*/?*) ;; *) CTC_NOTE="'$r' is not an owner/name slug"; return 0 ;; esac
  src="$(ctc_stage_for_tenant "$t")"
  case "$src" in
    rejected*) CTC_BEARER_SRC="$src"; CTC_NOTE="no credential for tenant $t (${src#rejected})"; return 0 ;;
  esac
  CTC_BEARER_SRC="$src"
  CTC_DOOR_CODE="$(_ctc_probe "$r")"
  rm -f "$CTC_TMP/bearer.hdr"
  case "$CTC_DOOR_CODE" in
    200) CTC_STATE=proven; CTC_TENANT="$t"; CTC_NOTE="proven: tenant $t owns $r (author-session door answered 200, bearer=$src)" ;;
    404) CTC_STATE=refuted; CTC_NOTE="tenant $t does not own $r (author-session door answered 404 under a token claiming it, bearer=$src)" ;;
    *)   CTC_NOTE="the author-session door answered HTTP $CTC_DOOR_CODE for $r (bearer=$src) -- which tenant owns it is UNKNOWN" ;;
  esac
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
