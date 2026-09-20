#!/usr/bin/env bash
# drain-closeout-spool.sh -- push the runner's UNACKED closeout rows to coord
# when the runner that owns them is gone.
#
# Phase R2.2 of plan
# 2026-08-28-closeout-has-no-durable-store-when-the-runner-is-offline.
#
# ── The failure this exists for ──────────────────────────────────────────────
# Every coord write door a session has routes through the local runner. When a
# session's writes fail, they are not lost -- the runner spools them to
# `~/.qontinui/runner/session-outbox.jsonl` and its own drain replays them
# later. That is a complete answer for a runner that comes back.
#
# It is not an answer for the case this dossier is actually about: the runner is
# GONE. Nothing else reads that file, so a closeout's gates and findings sit on
# disk, unsent, indefinitely -- and the session that wrote them has already
# reported them as recorded.
#
# ── Why this drainer NEVER writes the outbox or the cursor ───────────────────
# The obvious design -- mark rows drained, advance `<outbox>.cursor` -- fails
# twice over, and both failures are silent:
#
#   * `local_store.rs` `validate_cursor_locked` RESETS a cursor whose anchor no
#     longer matches the file it names. A cursor this script advanced is exactly
#     that shape, so the runner would reset it and replay anyway.
#   * A wedged-but-alive runner still holds its cursor IN MEMORY. Writing the
#     file races it, and the loser's view of "what has been sent" is wrong in
#     whichever direction happens to win.
#
# So this script is PUSH-ONLY. It reads, it POSTs, it prints, and it touches
# nothing. Safety comes from the SINK instead: coord's `findings::post`
# de-duplicates a live identical finding (Phase R2.1, qontinui-coord#2127) and
# `gates.rs` `register_gate_core` has had the same funnel for longer. A second
# replayer is therefore free rather than careful -- including the runner's own
# drain after a partial ACK, and including any future one.
#
# Until R2.1 DEPLOYS, a double drain duplicates findings. That is visible and
# supersedable, not silent, which is the direction the plan chose deliberately.
#
# ── What it sends ────────────────────────────────────────────────────────────
# Only the two closeout kinds, and only rows with no `acked_at`:
#
#   gate_registration -> POST $COORD_HTTP_URL/coord/work-units/<slug>/register-gate
#       Body rebuilt exactly as the runner's `coord_sync.rs`
#       `gate_registration_body` builds it: `predicate` and `phase_name`
#       forwarded when present; `continuation_spawn`, `clearance_audience` and
#       `gate_class` forwarded only when present AND non-null (a literal null is
#       a deserialize error on coord's non-Option `clearance_audience`).
#       `work_unit_slug` is a PATH segment and `work_unit_upsert` is the lazy
#       bootstrap -- neither belongs in the body.
#       On 404 `work_unit_not_found` WITH a `work_unit_upsert` hint present:
#       upsert once, retry once. Never more.
#
#   finding_posted    -> POST $COORD_HTTP_URL/coord/agent-findings
#       Payload forwarded VERBATIM.
#
# Any other `event_kind` is skipped and counted. This script is not a general
# outbox drain and must never become one: the other kinds are session lifecycle
# telemetry whose value expired with the session, and replaying them hours later
# would write a false history.
#
# ── Usage ────────────────────────────────────────────────────────────────────
#   drain-closeout-spool.sh --header-file <file> [--dry-run] [--outbox <path>]
#
#   --header-file   A file holding one `Authorization: Bearer <token>` line, as
#                   `curl -H @file` wants it. REQUIRED, and a FILE on purpose:
#                   process cmdlines are world-readable on this multi-session
#                   machine, so a bearer on argv leaks to every peer session.
#                   /gate's bootstrap-credential rung and coord-revive's L4/L5
#                   both already stage one. (Named, not numbered: rung numbers
#                   move whenever a rung is inserted and this file is never in
#                   that commit's diff -- check #37's whole subject.)
#   --dry-run       Print the table and send nothing.
#   --outbox        Drain only this file. Default: the primary's path plus every
#                   `instance-*/` sibling.
#
# Exit codes:
#   0  every eligible row was accepted (or there were none, or --dry-run)
#   2  usage error
#   3  no readable outbox -- a statement of ABSENCE about this box, never a
#      coord verdict
#   4  at least one row was REFUSED by coord (a 4xx; never retried)
#   5  at least one row could not be delivered (transport, 5xx) -- retryable
#
# Hermetic apart from the POSTs: no git, no coord credential minting, no writes.

set -uo pipefail

COORD_URL="${COORD_HTTP_URL:-https://coord.qontinui.io}"
HDR=""
DRY=0
OUTBOX_ARG=""

die_usage() { printf 'drain-closeout-spool.sh: %s\n' "$*" >&2; exit 2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --header-file) HDR="${2:-}"; shift 2 || die_usage "--header-file needs a path" ;;
    --dry-run)     DRY=1; shift ;;
    --outbox)      OUTBOX_ARG="${2:-}"; shift 2 || die_usage "--outbox needs a path" ;;
    -h|--help)     sed -n '2,/^set -uo/p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *)             die_usage "unknown argument: $1" ;;
  esac
done

[ "$DRY" = "1" ] || [ -n "$HDR" ] || die_usage "--header-file is required (or pass --dry-run)"
if [ -n "$HDR" ] && [ ! -r "$HDR" ]; then
  die_usage "--header-file $HDR is not readable"
fi

# A native curl.exe cannot open a POSIX mktemp path when MSYS pathconv is off,
# and it fails SILENTLY -- exit 0, the right http_code, the body written where
# bash never looks. Same conversion /gate stages its header with.
hdrp() { command -v cygpath >/dev/null 2>&1 && cygpath -w "$HDR" || printf '%s' "$HDR"; }

# ── Pick a JSON reader up front and fail LOUD ────────────────────────────────
# jq is ABSENT on the Windows operator box. With `jq ... 2>/dev/null` inline, a
# missing binary is indistinguishable from an empty file: every row would be
# skipped and this script would report "nothing to drain" over a full spool.
if command -v python3 >/dev/null 2>&1; then PY=python3
elif command -v python >/dev/null 2>&1; then PY=python
else
  echo "drain-closeout-spool: no python on PATH, so the outbox cannot be parsed. LOCAL fault -- this says nothing about coord or about whether rows are pending." >&2
  exit 3
fi

# ── Resolve the outbox roster ────────────────────────────────────────────────
# The primary's path is unscoped (`main.rs`, "session: resolved outbox path");
# a NAMED secondary runner gets its own `instance-<name>/` subdirectory via
# `instance::scope_path`, so a box that ever ran one has more than one spool.
# Draining only the primary silently leaves a secondary's closeout on disk.
RUNNER_DIR="${HOME:-.}/.qontinui/runner"
OUTBOXES=()
if [ -n "$OUTBOX_ARG" ]; then
  # An explicitly named spool that cannot be read is the same ABSENCE as no
  # spool at all. Admitting it to the roster would let the parser skip it in
  # silence and report "Nothing to drain" with exit 0 -- a clean-looking
  # verdict over a file nobody read.
  if [ -r "$OUTBOX_ARG" ] && [ -f "$OUTBOX_ARG" ]; then
    OUTBOXES+=("$OUTBOX_ARG")
  else
    echo "drain-closeout-spool: --outbox $OUTBOX_ARG is not a readable regular file (the default $RUNNER_DIR roster was NOT consulted)."
    echo "  This is a statement of ABSENCE about that path -- it is NOT evidence that a"
    echo "  closeout was delivered, and it is NOT a coord verdict."
    exit 3
  fi
else
  [ -r "$RUNNER_DIR/session-outbox.jsonl" ] && OUTBOXES+=("$RUNNER_DIR/session-outbox.jsonl")
  for d in "$RUNNER_DIR"/instance-*/; do
    [ -r "$d/session-outbox.jsonl" ] && OUTBOXES+=("$d/session-outbox.jsonl")
  done
fi

if [ "${#OUTBOXES[@]}" -eq 0 ]; then
  echo "drain-closeout-spool: no readable outbox under $RUNNER_DIR (and none named with --outbox)."
  echo "  This is a statement of ABSENCE about this box -- it is NOT evidence that a"
  echo "  closeout was delivered, and it is NOT a coord verdict. A runner that never"
  echo "  started here writes no spool."
  exit 3
fi

# ── Extract the eligible rows ────────────────────────────────────────────────
# One pass, in file order, so `seq` ordering is preserved per session -- a gate
# registration must not overtake the work-unit upsert that bootstraps it.
ROWS=$("$PY" - "${OUTBOXES[@]}" <<'PYEOF'
import json, sys

KINDS = ("gate_registration", "finding_posted")
out, skipped, unparseable = [], 0, 0
for path in sys.argv[1:]:
    try:
        fh = open(path, encoding="utf-8", errors="replace")
    except OSError:
        continue
    with fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except Exception:
                # A torn final line is the normal shape of a runner killed
                # mid-append. Counted, never guessed at.
                unparseable += 1
                continue
            if rec.get("acked_at") is not None:
                continue
            kind = rec.get("event_kind")
            if kind not in KINDS:
                skipped += 1
                continue
            out.append({
                "src": path,
                "session_id": rec.get("session_id"),
                "seq": rec.get("seq"),
                "event_kind": kind,
                "payload": rec.get("payload") or {},
            })
print(json.dumps({"rows": out, "skipped": skipped, "unparseable": unparseable}))
PYEOF
)

N=$("$PY" -c 'import json,sys; print(len(json.loads(sys.stdin.read())["rows"]))' <<<"$ROWS")
SKIPPED=$("$PY" -c 'import json,sys; print(json.loads(sys.stdin.read())["skipped"])' <<<"$ROWS")
TORN=$("$PY" -c 'import json,sys; print(json.loads(sys.stdin.read())["unparseable"])' <<<"$ROWS")

echo "drain-closeout-spool: ${#OUTBOXES[@]} outbox file(s); $N unacked closeout row(s); $SKIPPED non-closeout row(s) left alone; $TORN unparseable line(s)."
for o in "${OUTBOXES[@]}"; do echo "  spool: $o"; done
echo

if [ "$N" -eq 0 ]; then
  echo "Nothing to drain. NOTE: this script never writes the outbox, so a row it"
  echo "sent earlier is still here and still unacked until the RUNNER's own drain"
  echo "acks it. 'Nothing to drain' means no UNACKED closeout row, not 'no rows'."
  exit 0
fi

printf '%-18s %-38s %6s  %-6s %s\n' KIND SESSION SEQ HTTP RESULT

REFUSED=0
UNDELIVERED=0

post_json() {
  # post_json <url> <body-file> -> prints "<http_code>\t<body>"
  local url="$1" bodyfile="$2" resp code
  local outf; outf=$(mktemp); 
  local bp; bp=$(command -v cygpath >/dev/null 2>&1 && cygpath -w "$outf" || printf '%s' "$outf")
  local inp; inp=$(command -v cygpath >/dev/null 2>&1 && cygpath -w "$bodyfile" || printf '%s' "$bodyfile")
  # `-o`/`%{http_code}` rather than `-f`: this script READS the status itself,
  # and a 4xx must be reported as a refusal rather than collapsed into exit 22.
  code=$(curl -sS -o "$bp" -w '%{http_code}' -m 45 \
           -X POST "$url" -H @"$(hdrp)" -H 'Content-Type: application/json' \
           --data-binary @"$inp" 2>/dev/null)
  resp=$(head -c 400 "$outf" 2>/dev/null)
  rm -f "$outf"
  printf '%s\t%s' "${code:-000}" "$resp"
}

while IFS= read -r row; do
  KIND=$("$PY" -c 'import json,sys; print(json.loads(sys.stdin.read())["event_kind"])' <<<"$row")
  SESS=$("$PY" -c 'import json,sys; print(json.loads(sys.stdin.read())["session_id"] or "-")' <<<"$row")
  SEQ=$("$PY" -c 'import json,sys; print(json.loads(sys.stdin.read())["seq"])' <<<"$row")

  BODYF=$(mktemp); UPSERTF=$(mktemp); ROWF=$(mktemp)
  printf '%s' "$row" > "$ROWF"
  # The row travels by FILE, not on stdin: this script IS the heredoc's stdin,
  # so a `sys.stdin.read()` here would try to parse the program's own remaining
  # text. (It did, and every row came back UNSENDABLE with a JSONDecodeError
  # printed over the table.) Not on argv either -- a payload can be long and is
  # world-readable there.
  URL=$("$PY" - "$COORD_URL" "$BODYF" "$UPSERTF" "$ROWF" <<'PYEOF'
import json, sys
coord, bodyf, upsertf, rowf = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
row = json.load(open(rowf, encoding="utf-8"))
p = row["payload"]

def slug_of(payload):
    """The runner's own `gate_registration_slug`, verbatim: a single URL path
    segment or nothing. There is no percent-encoder on this path, so refusing
    is the honest answer -- a slug that cannot be a path segment can never
    succeed, which is why it is a permanent failure and not a retry."""
    s = payload.get("work_unit_slug")
    if not isinstance(s, str):
        return None
    s = s.strip()
    if not s or "/" in s or "?" in s or "#" in s or any(c.isspace() for c in s):
        return None
    return s

if row["event_kind"] == "finding_posted":
    open(bodyf, "w", encoding="utf-8").write(json.dumps(p))
    print(f"{coord}/coord/agent-findings")
else:
    slug = slug_of(p)
    if slug is None:
        print("")  # the caller reports UNSENDABLE
        sys.exit(0)
    body = {}
    for k in ("predicate", "phase_name"):
        if k in p:
            body[k] = p[k]
    for k in ("continuation_spawn", "clearance_audience", "gate_class"):
        if k in p and p[k] is not None:
            body[k] = p[k]
    open(bodyf, "w", encoding="utf-8").write(json.dumps(body))
    boot = p.get("work_unit_upsert")
    if isinstance(boot, dict):
        ub = {"slug": slug}
        for k in ("title", "status", "metadata", "by_actor"):
            if k in boot and boot[k] is not None:
                ub[k] = boot[k]
        open(upsertf, "w", encoding="utf-8").write(json.dumps(ub))
    print(f"{coord}/coord/work-units/{slug}/register-gate")
PYEOF
)

  if [ -z "$URL" ]; then
    printf '%-18s %-38s %6s  %-6s %s\n' "$KIND" "$SESS" "$SEQ" "-" \
      "UNSENDABLE (no usable work_unit_slug -- the register-gate URL cannot be built; permanent, not a retry)"
    REFUSED=1
    rm -f "$BODYF" "$UPSERTF" "$ROWF"; continue
  fi

  if [ "$DRY" = "1" ]; then
    printf '%-18s %-38s %6s  %-6s %s\n' "$KIND" "$SESS" "$SEQ" "dry" "would POST $URL"
    rm -f "$BODYF" "$UPSERTF" "$ROWF"; continue
  fi

  RES=$(post_json "$URL" "$BODYF"); CODE=${RES%%$'\t'*}; BODY=${RES#*$'\t'}

  # The ONE retry, and only this one: coord answers 404 work_unit_not_found for
  # a gate whose unit was never upserted, and the producer recorded the
  # bootstrap for exactly that case. Upsert once, retry once, never more.
  if [ "$KIND" = "gate_registration" ] && [ "$CODE" = "404" ] \
     && [ -s "$UPSERTF" ] && printf '%s' "$BODY" | grep -q 'work_unit_not_found'; then
    URES=$(post_json "$COORD_URL/coord/work-units/upsert" "$UPSERTF"); UCODE=${URES%%$'\t'*}
    if [ "${UCODE:0:1}" = "2" ]; then
      RES=$(post_json "$URL" "$BODYF"); CODE=${RES%%$'\t'*}; BODY=${RES#*$'\t'}
      BODY="(after work_unit upsert $UCODE) $BODY"
    else
      BODY="work_unit upsert answered $UCODE; register-gate not retried. $BODY"
    fi
  fi

  ID=$(printf '%s' "$BODY" | "$PY" -c '
import json,sys
try:
    d = json.loads(sys.stdin.read())
except Exception:
    print(""); raise SystemExit
if not isinstance(d, dict):
    print(""); raise SystemExit
for k in ("gate_id", "finding_id"):
    if d.get(k):
        print(d[k]); raise SystemExit
f = d.get("finding")
if isinstance(f, dict) and f.get("finding_id"):
    dedup = " deduplicated" if f.get("deduplicated") else ""
    print(str(f["finding_id"]) + dedup); raise SystemExit
print("")
' 2>/dev/null)

  case "${CODE:0:1}" in
    2) OUT="${ID:-accepted}" ;;
    4) OUT="REFUSED (4xx -- never retried): $(printf '%s' "$BODY" | tr -d '\n' | head -c 160)"; REFUSED=1 ;;
    *) OUT="UNDELIVERED (retryable): $(printf '%s' "$BODY" | tr -d '\n' | head -c 160)"; UNDELIVERED=1 ;;
  esac
  printf '%-18s %-38s %6s  %-6s %s\n' "$KIND" "$SESS" "$SEQ" "$CODE" "$OUT"
  rm -f "$BODYF" "$UPSERTF" "$ROWF"
done < <("$PY" -c '
import json,sys
for r in json.loads(sys.stdin.read())["rows"]:
    print(json.dumps(r))
' <<<"$ROWS")

echo
echo "NOTE: nothing was written to any outbox or cursor. These rows stay unacked"
echo "until the RUNNER acks them, so a later run of this script will show them"
echo "again -- and re-sending them is SAFE because coord's sinks de-duplicate"
echo "(findings: qontinui-coord#2127; gates: register_gate_core's funnel). A"
echo "second 'accepted' here is not a second row."

[ "$REFUSED" = "1" ] && exit 4
[ "$UNDELIVERED" = "1" ] && exit 5
exit 0
