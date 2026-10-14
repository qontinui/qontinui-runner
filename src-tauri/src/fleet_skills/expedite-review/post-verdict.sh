#!/usr/bin/env bash
# post-verdict.sh -- post ONE expedited-land independence verdict to coord.
#
# Plan 2026-10-13-coord-expedited-land-lever-for-agents, Phase 6 (S13, "The
# falsifier"). The /expedite-review skill calls this once, after its
# fresh-context reviewer has judged the head-pinned diff against the recorded
# claim. It is the skill's one verdict write (the credential library below may
# also mint a token when no static or runner token serves):
#
#   POST <coord>/pr-merge/agent/expedite/<request_id>/review/<gate_id>
#        {"verdict": "agree"|"dispute", "reason": "<text>"}
#
# coord appends one row to its verdict log. The verdict is a MEASUREMENT with
# no effect on any session: it restricts nobody and lands nothing. It counts
# only when it arrives from the device that claimed the review gate's
# continuation, under a credential that proves possession of that device, so
# it must be posted from the session the review gate dispatched (the gate id
# is $QONTINUI_GATE_ID there).
#
# CREDENTIAL. A device bearer for the tenant PROVEN to own the PR's repo, from
# the shared owner-tenant library (lib/coord-tenant-credential.sh, a
# byte-identical copy of scripts/lib/coord-tenant-credential.sh): a static
# $COORD_DEVICE_JWT / ~/.qontinui/coord-device-jwt claiming that tenant, else
# the local runner's device token (the rung a dispatched session normally
# takes: the runner advertises skill:expedite-review only while it holds a live
# device JWT). The library's last rung, the anonymous POST /agents/credential
# mint, is left ON because it is also how the library reads which tenants this
# device is bound to; a verdict posted under a minted token proves no device,
# and coord refuses it 403 (exit 5) rather than recording it. The bearer is
# staged in a 0600 header file and passed as `curl -H @file`, never on argv.
#
# Usage:
#   post-verdict.sh --request <uuid> --gate <uuid> --repo <owner/name>
#                   --verdict agree|dispute --reason-file <path>
#
# Optional env:
#   COORD_HTTP_URL   coord base (default https://coord.qontinui.io). Only an
#                    https or a loopback (127.0.0.1) base is accepted.
#   EXPEDITE_REVIEW_CURL  curl binary or a test stub (default curl).
#
# Exit codes:
#   0  recorded (coord answered 200 with recorded:true); stdout names
#      verdict_id and whether coord counted it
#   2  usage error (nothing sent)
#   3  coord answered 404 review_not_found: this request carries no review
#      gate with that id in the tenant (nothing recorded)
#   4  no credential for the owning tenant, or ownership UNKNOWN (nothing sent)
#   5  coord answered 403 credential_lacks_possession (nothing recorded)
#   6  anything else: coord unreachable, 5xx, 422, an unreadable answer. The
#      verdict is UNKNOWN -- not recorded as far as this script can tell.

set -uo pipefail

REQUEST=""; GATE=""; REPO=""; VERDICT=""; REASON_FILE=""

usage() {
  cat >&2 <<'USAGE'
Usage: post-verdict.sh --request <uuid> --gate <uuid> --repo <owner/name>
                       --verdict agree|dispute --reason-file <path>
USAGE
}
# An option given as the LAST argument has no value: refuse it with exit 2 and
# the usage text, rather than crash on `set -u`'s unbound $2 (exit 1).
need() { [ $# -ge 2 ] || { echo "error: $1 needs a value" >&2; usage; exit 2; }; }

while [ $# -gt 0 ]; do
  case "$1" in
    --request) need "$@"; REQUEST="$2"; shift 2 ;;
    --gate) need "$@"; GATE="$2"; shift 2 ;;
    --repo) need "$@"; REPO="$2"; shift 2 ;;
    --verdict) need "$@"; VERDICT="$2"; shift 2 ;;
    --reason-file) need "$@"; REASON_FILE="$2"; shift 2 ;;
    -h|--help) usage; exit 2 ;;
    *) echo "error: unknown argument '$1'" >&2; usage; exit 2 ;;
  esac
done

is_uuid() { [[ "${1:-}" =~ ^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$ ]]; }

is_uuid "$REQUEST" || { echo "error: --request must be a uuid (got '$REQUEST')" >&2; exit 2; }
is_uuid "$GATE" || { echo "error: --gate must be a uuid (got '$GATE'); in the dispatched session it is \$QONTINUI_GATE_ID" >&2; exit 2; }
[[ "$REPO" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*/[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || { echo "error: --repo must be owner/name (got '$REPO')" >&2; exit 2; }
case "$VERDICT" in agree|dispute) ;; *) echo "error: --verdict must be agree or dispute (got '$VERDICT')" >&2; exit 2 ;; esac
[ -n "$REASON_FILE" ] && [ -r "$REASON_FILE" ] || { echo "error: --reason-file must name a readable file" >&2; exit 2; }

COORD_URL="${COORD_HTTP_URL:-https://coord.qontinui.io}"
COORD_URL="${COORD_URL%/}"
# A stricter subset of the shapes the credential library accepts (_ctc_url_ok:
# it also admits a named loopback host): https, or http to 127.0.0.1, and no
# userinfo (`@`), so `http://127.0.0.1:1@elsewhere/` cannot pass as loopback.
_re_https='^https://[A-Za-z0-9.-]+(:[0-9]+)?(/[A-Za-z0-9._~/%-]*)?$'
_re_loop='^http://127\.0\.0\.1(:[0-9]+)?(/[A-Za-z0-9._~/%-]*)?$'
if ! [[ "$COORD_URL" =~ $_re_https ]] && ! [[ "$COORD_URL" =~ $_re_loop ]]; then
  echo "error: refusing to send a bearer to '$COORD_URL' (https or 127.0.0.1 only, no userinfo)" >&2
  exit 2
fi
CURL="${EXPEDITE_REVIEW_CURL:-curl}"

# Every interpreter call below runs with -I: the cwd may be a checkout of the
# repo under review, and its own json.py must never be imported.
PY=""
for _c in python3 python; do
  if "$_c" -I -c 'import sys; sys.exit(0 if sys.version_info[0] == 3 else 1)' >/dev/null 2>&1; then PY="$_c"; break; fi
done
[ -n "$PY" ] || { echo "error: no Python 3 on PATH (needed to build the JSON body)" >&2; exit 2; }

TMPD="$(mktemp -d)" || { echo "error: mktemp -d failed; nothing sent" >&2; exit 2; }
chmod 700 "$TMPD"
trap 'rm -rf "$TMPD"' EXIT

# First pass, before anything touches the network: the reason must be
# readable UTF-8, and a dispute must say why.
if ! "$PY" -I - "$VERDICT" "$REASON_FILE" <<'PYEOF'
import sys
verdict, path = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as f:
    reason = f.read().strip()
if verdict == "dispute" and not reason:
    sys.exit("a dispute must say why: the reason file is empty")
PYEOF
then
  echo "error: the reason is unusable (see above); nothing sent" >&2
  exit 2
fi

# The owner-tenant credential.
_pv_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
_pv_ctc="$_pv_dir/lib/coord-tenant-credential.sh"
export CTC_TMP="$TMPD" CTC_COORD_URL="$COORD_URL" CTC_CURL="${CTC_CURL:-$CURL}"
# shellcheck source=lib/coord-tenant-credential.sh
if ! { [ -r "$_pv_ctc" ] && . "$_pv_ctc" && declare -F ctc_owner_tenant >/dev/null 2>&1; }; then
  echo "error: lib/coord-tenant-credential.sh is not usable at $_pv_ctc (a LOCAL fault, not a coord verdict); nothing sent" >&2
  exit 4
fi
ctc_owner_tenant "$REPO"
if [ "$CTC_STATE" != proven ] && [ "${CTC_TRANSPORT:-0}" = 1 ]; then
  echo "UNKNOWN: coord did not answer the ownership probe for $REPO ($CTC_NOTE); nothing sent" >&2
  exit 6
fi
if [ "$CTC_STATE" != proven ] || [ ! -s "$TMPD/bearer.hdr" ]; then
  echo "UNKNOWN: no device credential for the tenant that owns $REPO ($CTC_STATE: $CTC_NOTE); nothing sent" >&2
  exit 4
fi

# A bearer minted by the anonymous rung proves no device: coord would refuse
# the verdict 403, so say so here and send nothing.
case "${CTC_BEARER_SRC:-}" in
  *mint*)
    echo "not recorded: the only credential for the owning tenant was minted anonymously ($CTC_BEARER_SRC), which proves no device; nothing sent" >&2
    exit 5 ;;
esac

# The body, built by Python so the reason text is escaped exactly once.
#
# The reason is posted verbatim to an append-only log the requester may read,
# and the reviewer read a diff the requester wrote -- so it is REDACTED, never
# refused (a refusal would let a requester suppress a dispute by planting a
# token-shaped fixture in its own diff): every value of this session's own
# secrets (the staged bearer, ~/.qontinui/coord-device-jwt, any environment
# variable named like a secret) and every credential SHAPE (a JWT, a PEM
# private key, GitHub / AWS / Slack token prefixes) becomes [redacted]. A reason
# over 4000 characters is cut to coord's cap (a model cannot count to 4000).
if ! "$PY" -I - "$VERDICT" "$REASON_FILE" "$TMPD/bearer.hdr" >"$TMPD/body.json" <<'PYEOF'
import json, os, re, sys
verdict, path, bearer_hdr = sys.argv[1], sys.argv[2], sys.argv[3]
with open(path, encoding="utf-8") as f:
    reason = f.read().strip()
# coord refuses every control character but newline and tab, and a refusal
# here would let text quoted from the requester's diff suppress the verdict:
# REPLACE them (U+FFFD), never refuse.
reason = reason.replace("\r\n", "\n").replace("\r", "\n")
reason = re.sub(r"[\x00-\x08\x0b-\x1f\x7f-\x9f]", "\ufffd", reason)
secrets = set()
try:
    with open(bearer_hdr, encoding="utf-8") as f:
        secrets.add(f.read().strip().split()[-1])
except (OSError, IndexError):
    pass
try:
    with open(os.path.join(os.path.expanduser("~"), ".qontinui", "coord-device-jwt"), encoding="utf-8") as f:
        secrets.add(f.read().strip())
except OSError:
    pass
name_re = re.compile(r"PASSWORD|PASSWD|SECRET|TOKEN|KEY|JWT|NONCE|CREDENTIAL|BEARER", re.I)
for name, value in os.environ.items():
    # 16+ characters and not an existing path or a URL, so an ordinary value
    # (a socket path, a repo name) is not blanked out. "Existing path", not
    # "starts with /": `/` is a base64 character, and a secret can start with it.
    if (
        name_re.search(name)
        and len(value) >= 16
        and "://" not in value
        and not (os.path.isabs(value) and os.path.exists(value))
    ):
        secrets.add(value)
n = 0
for secret in sorted((x for x in secrets if len(x) >= 8), key=len, reverse=True):
    if secret in reason:
        n += reason.count(secret)
        reason = reason.replace(secret, "[redacted]")
SHAPES = [
    r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]*",
    r"-----BEGIN [A-Z ]*PRIVATE KEY-----(?:.|\n)*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)",
    r"\bgh[pousr]_[A-Za-z0-9]{20,}",
    r"\bgithub_pat_[A-Za-z0-9_]{20,}",
    r"\bAKIA[0-9A-Z]{16}\b",
    r"\bxox[abprs]-[A-Za-z0-9-]{10,}",
]
for shape in SHAPES:
    reason, k = re.subn(shape, "[redacted]", reason)
    n += k
if n:
    print(f"note: {n} credential-shaped or secret value(s) in the reason were redacted", file=sys.stderr)
if verdict == "dispute" and not reason.replace("[redacted]", "").strip():
    reason = "[the reviewer's reason consisted only of redacted credential-shaped text]"
if len(reason) > 4000:
    print(f"note: reason was {len(reason)} characters; cut to coord's cap of 4000", file=sys.stderr)
    reason = reason[:4000]
json.dump({"verdict": verdict, "reason": reason}, sys.stdout)
PYEOF
then
  echo "error: could not build the verdict body (see above); nothing sent" >&2
  exit 2
fi

# curl is a native binary on Windows (Git Bash): hand it native paths, or an
# inherited MSYS_NO_PATHCONV=1 sends it a POSIX path it resolves elsewhere.
curl_path() { if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s' "$1"; fi; }
HDRPATH="$(curl_path "$TMPD/bearer.hdr")"
BODYPATH="$(curl_path "$TMPD/body.json")"
RESPPATH="$(curl_path "$TMPD/resp.json")"

hdrs=(-H @"$HDRPATH" -H "Content-Type: application/json")
_sid="${CLAUDE_CODE_SESSION_ID:-${CLAUDE_SESSION_ID:-}}"
if is_uuid "$_sid"; then
  hdrs+=(-H "X-Coord-Caller-Session: $_sid")
fi

URL="$COORD_URL/pr-merge/agent/expedite/$REQUEST/review/$GATE"
code="$("$CURL" -sS -o "$RESPPATH" -w '%{http_code}' --connect-timeout 10 --max-time 30 \
  -X POST "${hdrs[@]}" --data-binary @"$BODYPATH" "$URL" 2>"$TMPD/curl.err")"
rc=$?
if [ "$rc" -ne 0 ]; then
  echo "UNKNOWN: coord did not answer (curl exit $rc: $(head -c 300 "$TMPD/curl.err" | tr -d '\r')); the verdict is not known to be recorded" >&2
  exit 6
fi

summary="$("$PY" -I - "$TMPD/resp.json" <<'PYEOF' 2>/dev/null
import json, sys
try:
    with open(sys.argv[1], encoding="utf-8") as f:
        v = json.load(f)
except Exception:
    print("unreadable")
    sys.exit(0)
if not isinstance(v, dict):
    print("unreadable")
    sys.exit(0)
if v.get("recorded") is True:
    c = v.get("counted")
    counted = str(c).lower() if isinstance(c, bool) else "UNKNOWN"
    print(f"recorded verdict_id={v.get('verdict_id')} counted={counted}")
else:
    print("error=" + str(v.get("error", "?")))
PYEOF
)"

case "$code" in
  200)
    case "$summary" in
      recorded*) echo "$summary"; exit 0 ;;
      *) echo "UNKNOWN: coord answered 200 without recorded:true ($summary)" >&2; exit 6 ;;
    esac ;;
  404)
    # Only coord's OWN refusal is a fact about the request; a 404 from a coord
    # build without this route, or from a proxy, is UNKNOWN.
    [ "$summary" = "error=review_not_found" ] || { echo "UNKNOWN: coord answered HTTP 404 without review_not_found ($summary); the verdict is not known to be recorded" >&2; exit 6; }
    echo "not recorded: coord answered 404 ($summary) -- request $REQUEST carries no review gate $GATE in the owning tenant" >&2; exit 3 ;;
  403)
    [ "$summary" = "error=credential_lacks_possession" ] || { echo "UNKNOWN: coord answered HTTP 403 without credential_lacks_possession ($summary); the verdict is not known to be recorded" >&2; exit 6; }
    echo "not recorded: coord answered 403 ($summary) -- the credential does not prove possession of a device" >&2; exit 5 ;;
  *) echo "UNKNOWN: coord answered HTTP $code ($summary); the verdict is not known to be recorded" >&2; exit 6 ;;
esac
