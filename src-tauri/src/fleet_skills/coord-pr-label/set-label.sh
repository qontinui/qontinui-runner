#!/usr/bin/env bash
# coord-pr-label — declare or retract coord:* labels on a PR through coord's ONE
# label door (`POST|DELETE /coord/pr-labels`).
#
# THIS SCRIPT IS THE SHELL / CI FALLBACK. From a Claude Code session that has
# coord-mcp, call the MCP tools `coord_pr_label_set` / `coord_pr_label_unset`
# directly — that is the sanctioned path, and it needs no credential handling.
#
# What the door guarantees (plan 2026-08-27-coord-pr-label-write-path-single-door,
# dossier coord-pr-label-half-write): coord validates each label, canonicalizes
# a bare `<repo>#<n>` against your tenant's repos, writes GITHUB FIRST, then
# records the coord.pr_labels row as source='github', then syncs the dependency
# edges before it answers. A GitHub failure yields a `rejected[]` entry and NO
# row; a dependency edge that would close a cycle is undone on GitHub. There is
# no `gh` step and no separate coord step in this script any more, so there is
# no half-write it can leave behind — the seven-occurrence failure class the old
# two-step script produced ("label on GitHub, no edge in coord") is structurally
# gone, and so is the client-side validator mirror that drifted from coord five
# times: `--dry-run` asks the door to validate instead.
#
# Transport cascade — the first rung that ANSWERS wins; a 401/403 falls through:
#   1. The local runner's coord-mcp write forwarder:
#      <proxy-url>/pr-labels with the nonce from a runner-written .mcp.json
#      ($PWD, its parent, sibling repos, $QONTINUI_ROOT). The runner injects a
#      fresh device JWT upstream, so this rung needs no credential in your hands.
#   2. coord directly — ${COORD_HTTP_URL:-${COORD_URL:-https://coord.qontinui.io}}/coord/pr-labels
#      with a bearer from $COORD_AGENT_JWT, else $COORD_DEVICE_JWT, else the
#      file ~/.qontinui/coord-device-jwt.
#   Neither answered: exit 4, and the message says that NOTHING was written on
#   either side — which is true, because there is nothing this script writes
#   itself.
#
# Exit codes:
#   0  every requested label was declared / retracted
#   1  the door answered and REFUSED some or all labels (`rejected:` lines say why);
#      nothing partial was left behind for a refused label
#   2  usage error, or no JSON tool (python3/python/jq) on PATH
#   4  no transport answered, OR the bearer rung refused its destination (a
#      coord base that is neither https nor loopback http), OR no temp dir —
#      NOTHING was written, on either side. This is the only code that promises
#      that, which is why 5 exists.
#   5  a door ANSWERED and the call failed anyway (a 5xx, an unexpected 4xx, or a
#      body that is not JSON). NOT the same as 4: coord writes GitHub FIRST, so a
#      500 from the row INSERT lands after the label is already on the PR. Re-run
#      with --dry-run, or look at the PR, before assuming nothing happened.

set -euo pipefail

REPO=""
PR=""
LABELS=()
UNSET=""
MODE="merge"
DRY_RUN=0
RAW_JSON=0

usage() {
  cat <<'EOF'
Usage: set-label.sh --repo <owner/name> --pr <n> --label "coord:<key>[=<value>]" [--label ...]
                    [--replace] [--dry-run] [--json]
       set-label.sh --repo <owner/name> --pr <n> --unset "coord:<key>[=<value>]" [--json]

Options:
  --label <l>   A coord:* label to declare. Repeatable. With --replace, the
                posted set becomes the PR's COMPLETE author-settable coord:*
                declaration — every other author-settable label is retracted
                from GitHub and coord. --replace with no --label is a total
                retraction.
  --unset <l>   Retract one label from both stores (GitHub, then coord), then
                re-sync the dependency edges.
  --replace     Set semantics (see --label). Default is additive (`merge`).
  --dry-run     Ask coord to validate + canonicalize + check the repo is yours,
                writing nothing. Replaces the old local validator.
  --json        Print the door's raw JSON response after the summary lines.

Env (only for the direct rung; the runner forwarder rung needs none of it):
  COORD_HTTP_URL / COORD_URL   coord base. Default https://coord.qontinui.io.
  COORD_AGENT_JWT, COORD_DEVICE_JWT, ~/.qontinui/coord-device-jwt   a bearer.

Grammar (validated by coord, not here): coord:upstream-of=[<owner>/]<repo>#<n>,
coord:downstream-of=[<owner>/]<repo>#<n>, coord:stacked-on=#<n>|[<owner>/]<repo>#<n>,
coord:requires-tag=<pattern>, coord:merge-strategy=squash|rebase|merge, and the
flags coord:credibility-override, coord:migrate-repair. coord:priority,
coord:blocked, coord:experimental and coord-set labels are refused --
coord:blocked / coord:experimental are retired holds (they never held a PR), so
declare one and the door says so. --unset still retracts one already on a PR.
EOF
}

need_value() {
  if [[ $# -lt 2 ]]; then
    echo "error: $1 needs a value" >&2
    usage >&2
    exit 2
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)    need_value "$@"; REPO="$2";      shift 2 ;;
    --pr)      need_value "$@"; PR="$2";        shift 2 ;;
    --label)   need_value "$@"; LABELS+=("$2"); shift 2 ;;
    --unset)   need_value "$@"; UNSET="$2";     shift 2 ;;
    --replace) MODE="replace"; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --json)    RAW_JSON=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown arg: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ -z "$REPO" || -z "$PR" ]]; then
  echo "error: --repo and --pr are required" >&2
  usage >&2
  exit 2
fi
if [[ -n "$UNSET" && ${#LABELS[@]} -gt 0 ]]; then
  echo "error: --unset and --label are exclusive (one retraction per call)" >&2
  exit 2
fi
# The door has no dry_run on its retract verb, so accepting the flag here would
# silently perform the retraction the caller asked to rehearse. A destructive
# verb never ignores a flag: refuse instead.
if [[ -n "$UNSET" && "$DRY_RUN" -eq 1 ]]; then
  echo "error: --dry-run does not apply to --unset — the retract verb has no dry run," >&2
  echo "       and ignoring the flag would retract the label you asked to rehearse." >&2
  exit 2
fi
if [[ -z "$UNSET" && ${#LABELS[@]} -eq 0 && "$MODE" != "replace" ]]; then
  echo "error: nothing to do — pass --label (repeatable), --unset, or --replace with no labels" >&2
  usage >&2
  exit 2
fi
if ! [[ "$PR" =~ ^[0-9]+$ ]]; then
  echo "error: --pr must be a positive integer, got \"$PR\"" >&2
  exit 2
fi

# ----- JSON tool -------------------------------------------------------------
# python3, else python (checked by OUTPUT — Windows ships App Execution Alias
# stubs that resolve and exit non-zero), else jq. The body and the response are
# both JSON, so one of them is required; say so rather than guessing.
JSON_PY=""
for c in python3 python; do
  if command -v "$c" >/dev/null 2>&1 \
     && [[ "$("$c" -c 'import json;print(1)' </dev/null 2>/dev/null | tr -d '\r\n')" == "1" ]]; then
    JSON_PY="$c"; break
  fi
done
HAVE_JQ=0
command -v jq >/dev/null 2>&1 && HAVE_JQ=1
if [[ -z "$JSON_PY" && "$HAVE_JQ" -eq 0 ]]; then
  echo "error: need python3, python, or jq on PATH to build and read JSON" >&2
  exit 2
fi

# ----- request body ------------------------------------------------------------
if [[ -n "$UNSET" ]]; then
  METHOD="DELETE"
  if [[ -n "$JSON_PY" ]]; then
    BODY=$("$JSON_PY" -c 'import json,sys; print(json.dumps({"repo":sys.argv[1],"pr_number":int(sys.argv[2]),"label":sys.argv[3]}))' "$REPO" "$PR" "$UNSET")
  else
    BODY=$(jq -cn --arg r "$REPO" --argjson n "$PR" --arg l "$UNSET" '{repo:$r,pr_number:$n,label:$l}')
  fi
else
  METHOD="POST"
  if [[ -n "$JSON_PY" ]]; then
    BODY=$("$JSON_PY" -c 'import json,sys; print(json.dumps({"repo":sys.argv[1],"pr_number":int(sys.argv[2]),"mode":sys.argv[3],"dry_run":sys.argv[4]=="1","labels":sys.argv[5:]}))' "$REPO" "$PR" "$MODE" "$DRY_RUN" "${LABELS[@]+"${LABELS[@]}"}")
  else
    BODY=$(jq -cn --arg r "$REPO" --argjson n "$PR" --arg m "$MODE" --argjson d "$([[ $DRY_RUN == 1 ]] && echo true || echo false)" \
      '{repo:$r,pr_number:$n,mode:$m,dry_run:$d,labels:$ARGS.positional}' --args "${LABELS[@]+"${LABELS[@]}"}")
  fi
fi

# ----- one request; returns "<code>\n<body>" via globals ------------------------
TMPD="$(mktemp -d)" || { echo "error: mktemp -d failed" >&2; exit 4; }
trap 'rm -rf "$TMPD"' EXIT

# A native curl opens -H @file itself; on an MSYS box the POSIX path must cross
# as a Windows one -- the same helper handoff-stuck-pr.sh / coord-revive use.
curl_path() { if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s' "$1"; fi; }

# bearer_url_ok -> 0 iff <url> is https or loopback http: a device JWT is never
# sent in clear text to an arbitrary host. $COORD_HTTP_URL / $COORD_URL are
# ordinary per-session env vars, so the destination is attacker-influencable
# and the bearer rung must be gated on it.
bearer_url_ok() {
  case "$1" in *@*) return 1 ;; esac  # userinfo would retarget the host
  case "$1" in
    https://*|http://127.0.0.1|http://127.0.0.1:[0-9]*|http://127.0.0.1/*|http://localhost|http://localhost:[0-9]*|http://localhost/*|"http://[::1]"*) return 0 ;;
  esac
  return 1
}

HTTP_CODE=""
RESPONSE=""
# $1 url, $2 header name, $3 header value. Returns 0 when curl completed (any
# HTTP code), 1 when the transport itself failed (connection refused, timeout).
# The credential ($3 -- a runner nonce on rung 1, a device/agent JWT on rung 2)
# is staged in a 0600 header FILE and never passed on argv, where `ps` would
# expose it to every process on the box for the life of the call.
# curl reads `-H @file` LINE BY LINE, so a newline inside the value would become
# a second header rather than a mangled one. Every credential reaching here is
# scrubbed of whitespace at its source (header_safe), and this is the assertion
# that it happened -- refuse rather than emit a header we did not intend.
send() {
  local raw
  # Only CR/LF are the injection vector -- a header value legitimately contains
  # spaces (`Bearer <token>` is two words).
  case "$3" in *[$'\r\n']*) echo "error: refusing a header value containing a newline" >&2; return 1 ;; esac
  ( umask 077; printf '%s: %s\n' "$2" "$3" > "$TMPD/req.hdr" )
  raw=$(curl -sS -m 30 -w $'\n%{http_code}' -X "$METHOD" "$1" \
      -H "@$(curl_path "$TMPD/req.hdr")" -H 'Content-Type: application/json' -d "$BODY" 2>/dev/null) || {
    rm -f "$TMPD/req.hdr"; return 1
  }
  # The credential does not outlive the request it was staged for.
  rm -f "$TMPD/req.hdr"
  HTTP_CODE=${raw##*$'\n'}
  RESPONSE=${raw%$'\n'*}
  return 0
}

# header_safe <value> -> the value with all whitespace removed, or empty when it
# held any. A JWT and a runner nonce are both whitespace-free by construction,
# so a candidate that is not is malformed and is skipped rather than repaired.
header_safe() {
  case "$1" in
    *[$' \t\r\n']*) printf '' ;;
    *) printf '%s' "$1" ;;
  esac
}

# ----- rung 1: the runner's coord-mcp write forwarder -------------------------
# Candidate .mcp.json files: own cwd, its parent, sibling repos, $QONTINUI_ROOT.
# A runner-written coord-mcp entry is proxy-shaped: a loopback `url` ending in
# /coord-mcp plus the nonce under `Authorization: Bearer <nonce>` (configs
# written after the Phase 2 header move) or the legacy `X-Coord-Mcp-Proxy-Key`.
# `Authorization` wins when both are present, mirroring the runner's own
# request-side resolver.
declare -a CANDIDATES=("$PWD/.mcp.json" "$PWD/../.mcp.json")
for f in "$PWD"/../*/.mcp.json; do CANDIDATES+=("$f"); done
if [[ -n "${QONTINUI_ROOT:-}" ]]; then
  CANDIDATES+=("$QONTINUI_ROOT/.mcp.json")
  for f in "$QONTINUI_ROOT"/*/.mcp.json; do CANDIDATES+=("$f"); done
fi

read_mcp_entry() {
  # Prints "<url>\t<header>\t<value>" or nothing.
  local cfg="$1"
  if [[ -n "$JSON_PY" ]]; then
    "$JSON_PY" - "$cfg" <<'PY' 2>/dev/null
import json, sys
try:
    d = json.load(open(sys.argv[1], encoding="utf-8"))
except Exception:
    sys.exit(0)
c = ((d.get("mcpServers") or {}).get("coord-mcp") or {})
url = (c.get("url") or "").rstrip("/")
h = c.get("headers") or {}
if not url.endswith("/coord-mcp"):
    sys.exit(0)
if str(h.get("Authorization") or ""):
    print(f"{url}\tAuthorization\t{h['Authorization']}")
elif str(h.get("X-Coord-Mcp-Proxy-Key") or ""):
    print(f"{url}\tX-Coord-Mcp-Proxy-Key\t{h['X-Coord-Mcp-Proxy-Key']}")
PY
  else
    jq -r '(.mcpServers["coord-mcp"] // {}) as $c
      | ($c.url // "" | rtrimstr("/")) as $u
      | ($c.headers // {}) as $h
      | if ($u | endswith("/coord-mcp")) | not then empty
        elif (($h.Authorization // "") | tostring) != "" then "\($u)\tAuthorization\t\($h.Authorization)"
        elif (($h["X-Coord-Mcp-Proxy-Key"] // "") | tostring) != "" then "\($u)\tX-Coord-Mcp-Proxy-Key\t\($h["X-Coord-Mcp-Proxy-Key"])"
        else empty end' < "$cfg" 2>/dev/null
  fi
}

ANSWERED=""
TRIED=()
SEEN_CFG=""
for cfg in "${CANDIDATES[@]}"; do
  [[ -f "$cfg" ]] || continue
  # `$PWD/../*/.mcp.json` re-lists $PWD's own file; probe each file once.
  real=$(cd "$(dirname "$cfg")" 2>/dev/null && pwd -P)/$(basename "$cfg")
  case "$SEEN_CFG" in *"|$real|"*) continue ;; esac
  SEEN_CFG="$SEEN_CFG|$real|"
  entry=$(read_mcp_entry "$cfg") || true
  [[ -n "$entry" ]] || continue
  IFS=$'\t' read -r PURL PHDR PKEY <<<"$entry"
  TRIED+=("forwarder:$PURL/pr-labels")
  if send "$PURL/pr-labels" "$PHDR" "$PKEY"; then
    case "$HTTP_CODE" in
      401|403) continue ;;                       # dead or foreign nonce — next candidate
      404)
        # A runner built before the pr-labels forwarder route 404s here; coord's
        # own 404 for a repo outside the tenant carries a typed body. Only the
        # latter is an answer.
        if [[ "$RESPONSE" == *repo_not_found_in_tenant_scope* ]]; then ANSWERED="forwarder $PURL"; break; fi
        continue ;;
      5*)
        # A 5xx carrying a RUNNER-originated code is the runner failing, not
        # coord answering — `COORD_MCP_PROXY_TENANT_UNRESOLVABLE` is a 503 from a
        # missing or malformed ~/.qontinui/machine.json, and
        # `COORD_WRITE_PROXY_UPSTREAM_UNREACHABLE` is a 502 because the runner
        # could not reach coord at all. Rung 2 exists for exactly that runner, so
        # fall through instead of stopping on it. A 5xx that coord itself
        # produced carries neither code and IS an answer.
        case "$RESPONSE" in
          *COORD_MCP_PROXY_*|*COORD_WRITE_PROXY_*) continue ;;
          *) ANSWERED="forwarder $PURL"; break ;;
        esac ;;
      *) ANSWERED="forwarder $PURL"; break ;;
    esac
  fi
done

# ----- rung 2: coord directly, with a bearer this shell holds -----------------
if [[ -z "$ANSWERED" ]]; then
  COORD_BASE="${COORD_HTTP_URL:-${COORD_URL:-https://coord.qontinui.io}}"
  COORD_BASE="${COORD_BASE%/}"
  # jwt-cascade-selection: every bearer below is PROBED against coord and a 401/403
  # falls through to the next candidate, so a stale static $COORD_AGENT_JWT or
  # $COORD_DEVICE_JWT cannot SHADOW the file token or the runner mint behind it
  # (#366); no local `exp` decode is needed on this path. Declared for
  # scripts/lint-jwt-cascade-parity.py check E.
  TOKENS=()
  if ! bearer_url_ok "$COORD_BASE"; then
    # Not a fall-through: there is no next rung, and sending the bearer anyway
    # is the harm. Say so rather than reporting "no door answered".
    echo "error: refusing to send a bearer to $COORD_BASE — neither https nor loopback http." >&2
    echo "       \$COORD_HTTP_URL / \$COORD_URL set the destination; a device JWT is never" >&2
    echo "       sent in clear text to an arbitrary host. NOTHING was written." >&2
    exit 4
  fi
  # Each candidate is scrubbed at its source: an env var can carry a newline,
  # and `-H @file` is line-oriented, so an unscrubbed value would inject a
  # second header rather than merely fail. A malformed candidate is SKIPPED,
  # never repaired -- a JWT with whitespace in it is not the token you meant.
  t=$(header_safe "${COORD_AGENT_JWT:-}");  [[ -n "$t" ]] && TOKENS+=("$t")
  t=$(header_safe "${COORD_DEVICE_JWT:-}"); [[ -n "$t" ]] && TOKENS+=("$t")
  if [[ -r "$HOME/.qontinui/coord-device-jwt" ]]; then
    t=$(header_safe "$(tr -d '\r\n' < "$HOME/.qontinui/coord-device-jwt")")
    [[ -n "$t" ]] && TOKENS+=("$t")
  fi
  for tok in "${TOKENS[@]+"${TOKENS[@]}"}"; do
    TRIED+=("bearer:$COORD_BASE/coord/pr-labels")
    if send "$COORD_BASE/coord/pr-labels" "Authorization" "Bearer $tok"; then
      case "$HTTP_CODE" in
        401|403) continue ;;
        *) ANSWERED="direct $COORD_BASE"; break ;;
      esac
    fi
  done
fi

if [[ -z "$ANSWERED" ]]; then
  {
    echo "error: no coord door answered — NOTHING was written, on GitHub or in coord."
    if [[ ${#TRIED[@]} -eq 0 ]]; then
      echo "       No runner-written .mcp.json with a coord-mcp entry was found near \$PWD, and"
      echo "       no bearer is set (\$COORD_AGENT_JWT / \$COORD_DEVICE_JWT / ~/.qontinui/coord-device-jwt)."
    else
      echo "       tried (each unreachable or 401/403):"
      printf '         %s\n' "${TRIED[@]}"
    fi
    echo "       From a Claude Code session, call the MCP tool coord_pr_label_set / coord_pr_label_unset"
    echo "       instead; from a shell, run /coord-revive to find a live door or export a bearer."
  } >&2
  exit 4
fi

# ----- render the door's answer -------------------------------------------------
RENDER_PY=$(cat <<'PY'
import json, os, sys
# Every line this renderer prints carries an em dash (U+2014). Python picks its
# stream encoding from the console code page, so on a Windows host that is
# cp1252 and the dash goes out as the single byte 0x97 -- which a caller
# grepping for the UTF-8 spelling (e2 80 94) never matches. The fleet's guard
# suites do exactly that, and this is what made
# `rejected: "coord:nope" - unknown coord:* label key` fail on windows-latest
# while passing on linux. Reconfigure rather than delete the glyph: the dash is
# the separator the renderer's own contract is written around.
# errors="replace" for the same reason the linters use it -- a mangled glyph
# costs legibility, a raised UnicodeEncodeError costs the verdict.
for _stream in (sys.stdout, sys.stderr):
    _reconfigure = getattr(_stream, "reconfigure", None)
    if _reconfigure is not None:
        try:
            _reconfigure(encoding="utf-8", errors="replace")
        except (ValueError, OSError):
            pass
method, code, door = sys.argv[1], sys.argv[2], sys.argv[3]
raw = os.environ.get("RESP_JSON", "")
try:
    d = json.loads(raw)
except Exception:
    print(f"error: HTTP {code} from {door} with a non-JSON body: {raw[:400]}", file=sys.stderr)
    # 5, not 4: a door answered. Whether it wrote anything is UNKNOWN.
    sys.exit(5)
# The typed 404: coord considered the request and refused it outright because
# the repo is not in the acting tenant's scope. It is an ANSWER and a REFUSAL,
# so it exits 1 -- never into the success renderer, whose body it does not have.
# `acting_tenant_id` is echoed because it is the fact that separates "I typed
# the repo wrong" from "my credential is for another tenant".
if code == "404":
    print(f"rejected: {d.get('repo')} — {d.get('detail') or d.get('error')}", file=sys.stderr)
    if d.get("acting_tenant_id"):
        print(f"       acting tenant: {d['acting_tenant_id']} — check THAT before the repo name.",
              file=sys.stderr)
    sys.exit(1)
# ONLY a 2xx is success from here, plus 422 -- the code the door uses to say
# "everything you posted was refused", whose body IS the normal response shape
# and renders below as `rejected:` lines. Everything else -- 400 from
# `tenant_from_auth` when the bearer carries no tenant claim, 409, 413, 429,
# every 5xx -- is a FAILURE. The test was `startswith(("2","4"))`, which let
# 400/409/429 fall into the success renderer: a POST printed nothing and exited
# 0, and a DELETE printed `ok: retracted "None" from None#None`.
if not (code.startswith("2") or code == "422"):
    print(f"error: HTTP {code} from {door}: {json.dumps(d)[:600]}", file=sys.stderr)
    if method == "POST":
        print("       The door writes GitHub FIRST, so this does NOT mean nothing happened -- "
              "check the PR, or re-run with --dry-run.", file=sys.stderr)
    sys.exit(5)
if method == "DELETE":
    print(f"ok: retracted \"{d.get('label')}\" from {d.get('repo')}#{d.get('pr_number')} "
          f"(GitHub + coord; coord row existed: {str(d.get('deleted')).lower()}) via {door}")
    sys.exit(0)
repo, pr = d.get("repo"), d.get("pr_number")
if d.get("dry_run"):
    print(f"ok: dry run — coord accepts {len(d.get('valid') or [])} label(s) for {repo}#{pr}: "
          + ", ".join(d.get("valid") or []) + " (nothing sent to GitHub or written)")
for l in (d.get("github") or {}).get("added") or []:
    print(f"ok: declared \"{l}\" on {repo}#{pr} — on GitHub and in coord (source=github), edges synced")
for l in (d.get("github") or {}).get("removed") or []:
    print(f"ok: retracted \"{l}\" from {repo}#{pr} (mode=replace or cycle undo)")
rej = d.get("rejected") or []
for r in rej:
    cyc = r.get("cycle") or []
    extra = " — cycle: " + " -> ".join(f"{c['repo']}#{c['pr_number']}" for c in cyc) if cyc else ""
    print(f"rejected: \"{r.get('label')}\" — {r.get('reason')}{extra}", file=sys.stderr)
if rej:
    print(f"note: {len(rej)} label(s) refused by coord; nothing partial was left behind for them.", file=sys.stderr)
    sys.exit(1)
sys.exit(0)
PY
)

render() {
  if [[ -n "$JSON_PY" ]]; then
    RESP_JSON="$RESPONSE" "$JSON_PY" -c "$RENDER_PY" "$METHOD" "$HTTP_CODE" "$ANSWERED"
  else
    # jq-only render: coarse but honest.
    echo "$RESPONSE" | jq -r --arg m "$METHOD" --arg door "$ANSWERED" '
      if $m == "DELETE" then "ok: retracted \(.label) from \(.repo)#\(.pr_number) via \($door)"
      else (
        (if .dry_run then ["ok: dry run — valid: \(.valid | join(", "))"] else [] end)
        + ((.github.added // []) | map("ok: declared \(.) — on GitHub and in coord"))
        + ((.github.removed // []) | map("ok: retracted \(.)"))
        + ((.rejected // []) | map("rejected: \(.label) — \(.reason)"))
      ) | .[] end'
    if [[ "$METHOD" == "POST" ]] && echo "$RESPONSE" | jq -e '(.rejected // []) | length > 0' >/dev/null; then
      return 1
    fi
  fi
}

RC=0
render || RC=$?
if [[ "$RAW_JSON" -eq 1 ]]; then
  echo "$RESPONSE"
fi
exit "$RC"
