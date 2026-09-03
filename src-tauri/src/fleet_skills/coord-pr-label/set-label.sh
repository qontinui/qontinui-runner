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
#   1  the door answered and refused some or all labels (`rejected:` lines say why)
#   2  usage error, or no JSON tool (python3/python/jq) on PATH
#   4  no transport answered (nothing was written anywhere)

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
flags coord:blocked, coord:experimental, coord:credibility-override,
coord:migrate-repair. coord:priority and coord-set labels are refused.
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
HTTP_CODE=""
RESPONSE=""
# $1 url, $2 header name, $3 header value. Returns 0 when curl completed (any
# HTTP code), 1 when the transport itself failed (connection refused, timeout).
send() {
  local raw
  if ! raw=$(curl -sS -m 30 -w $'\n%{http_code}' -X "$METHOD" "$1" \
      -H "$2: $3" -H 'Content-Type: application/json' -d "$BODY" 2>/dev/null); then
    return 1
  fi
  HTTP_CODE=${raw##*$'\n'}
  RESPONSE=${raw%$'\n'*}
  return 0
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
      *) ANSWERED="forwarder $PURL"; break ;;
    esac
  fi
done

# ----- rung 2: coord directly, with a bearer this shell holds -----------------
if [[ -z "$ANSWERED" ]]; then
  COORD_BASE="${COORD_HTTP_URL:-${COORD_URL:-https://coord.qontinui.io}}"
  COORD_BASE="${COORD_BASE%/}"
  TOKENS=()
  [[ -n "${COORD_AGENT_JWT:-}" ]] && TOKENS+=("$COORD_AGENT_JWT")
  [[ -n "${COORD_DEVICE_JWT:-}" ]] && TOKENS+=("$COORD_DEVICE_JWT")
  if [[ -r "$HOME/.qontinui/coord-device-jwt" ]]; then
    t=$(tr -d '\r\n' < "$HOME/.qontinui/coord-device-jwt")
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
method, code, door = sys.argv[1], sys.argv[2], sys.argv[3]
raw = os.environ.get("RESP_JSON", "")
try:
    d = json.loads(raw)
except Exception:
    print(f"error: HTTP {code} from {door} with a non-JSON body: {raw[:400]}", file=sys.stderr)
    sys.exit(4)
if not code.startswith(("2", "4")) or code in ("401", "403", "404"):
    print(f"error: HTTP {code} from {door}: {json.dumps(d)[:600]}", file=sys.stderr)
    sys.exit(1 if code == "404" else 4)
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
