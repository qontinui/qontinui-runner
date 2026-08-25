#!/usr/bin/env bash
# coord-pr-label — set a coord:* label on a PR via gh + record in coord.pr_labels.
#
# Phase 2 D2.6 of the PR Merge Orchestrator
# (qontinui-dev-notes/plans/2026-05-21-pr-merge-orchestrator-design.md).
#
# Validates the label against the `coord:*` namespace, calls
# `gh pr edit <pr> --add-label "<label>"`, then POSTs the same label to
# coord's `POST /pr-merge/labels` so the row in `coord.pr_labels`
# carries `source='coord_skill'` + tenant resolved from the caller's
# agent_id (= the agent_worktrees row's tenant_id).

set -euo pipefail

REPO=""
PR=""
LABEL=""
DRY_RUN=0

usage() {
  cat <<'EOF'
Usage: set-label.sh --repo <owner/name> --pr <n> --label "coord:<key>[=<value>]"
                    [--dry-run]

Options:
  --dry-run          Validate the label (namespace grammar + GitHub's 50-char
                     label-name ceiling) and exit. Nothing is sent to GitHub or
                     coord, and QONTINUI_AGENT_ID is not required.

Required env:
  QONTINUI_AGENT_ID  — the spawning agent's UUID. Set by the agent-spawn
                       flow; if absent the skill exits with an error.

Optional env:
  COORD_URL          — coord base URL. Default http://localhost:9870.

Examples:
  set-label.sh --repo qontinui/qontinui-coord --pr 75 \
      --label "coord:upstream-of=qontinui/qontinui-schemas#42"
  set-label.sh --repo qontinui/qontinui-coord --pr 75 --label coord:blocked
EOF
}

# ----- arg parse --------------------------------------------------------------

# `shift 2` on a valueless flag fails under `set -e` and exits 1 with NO
# message at all, so check arity explicitly and say which flag is bare.
need_value() {
  if [[ $# -lt 2 ]]; then
    echo "error: $1 needs a value" >&2
    usage >&2
    exit 2
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)  need_value "$@"; REPO="$2";  shift 2 ;;
    --pr)    need_value "$@"; PR="$2";    shift 2 ;;
    --label) need_value "$@"; LABEL="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown arg: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ -z "$REPO" || -z "$PR" || -z "$LABEL" ]]; then
  echo "error: --repo, --pr, --label are all required" >&2
  usage >&2
  exit 2
fi

COORD_URL="${COORD_URL:-http://localhost:9870}"

# ----- validate against the coord:* namespace --------------------------------
# Mirrors qontinui-coord/src/pr_merge/labels_routes.rs::validate_label.
# Keeping the two in sync is a Phase 2 D2.6 requirement; the doc at
# qontinui-dev-notes/docs/coord/pr-merge-labels.md is the spec.
#
# Last reconciled against coord `origin/main` @ da36d08d (2026-08-22), which
# closed four drifts this mirror had accumulated. Cite the REF, not a local
# checkout, when you re-sync: the arms below are ordered as coord orders them,
# because order is load-bearing (a bespoke arm must precede the generic
# `parameterised labels need "=value"` fallthrough, or it never fires).

# Mirrors `labels_routes.rs::repo_segments_well_formed` -- a `<owner>/<repo>`
# value must have BOTH segments non-empty. Without this the skill green-lit
# `coord:upstream-of=/repo#1` and `coord:upstream-of=qontinui/#1`, which coord
# then refused at the write surface: a pre-flight that says "valid" for a label
# the server rejects is worse than no pre-flight.
repo_segments_well_formed() {
  local repo="$1"
  case "$repo" in
    */*) [[ -n "${repo%%/*}" && -n "${repo#*/}" ]]; return $? ;;
    *)   return 0 ;;
  esac
}

# Mirrors Rust `n.parse::<i32>()` in the dep-label / stacked-on arms.
# `^[0-9]+$` was NOT equivalent and diverged BOTH ways: it accepted
# `#2147483648` and `#99999999999999999999` (which coord rejects as i32
# overflow -- the same green-light-then-server-refuses failure that
# `repo_segments_well_formed` exists to prevent), and it rejected `#-1`,
# `#+1` and `#-2147483648` (which coord accepts).
#
# Whether a NEGATIVE pr_number ought to be legal is a separate question,
# and the answer is not this file's to give: change `labels_routes.rs`
# first and re-mirror. A mirror that "improves on" its source is drift.
#
# Leading zeros are the trap: bare `(( 007 ))` octal-parses, and `(( 008 ))`
# is a hard error. Three mechanisms interact below, and their relationship
# is NOT symmetric -- an earlier draft of this comment called the first two
# "redundant with each other", which is false in one direction:
#
#   * `10#` is DEFENCE-IN-DEPTH, fully covered by the strip loop. After
#     stripping there is never a leading zero left to octal-parse, so
#     removing `10#` alone drifts from `parse::<i32>()` on ZERO inputs.
#
#   * The strip loop is LOAD-BEARING TWICE. Beyond octal, it normalises
#     length BEFORE the `<= 10` guard runs -- and that ordering is the
#     whole point. Without it, `+00000000000` (11 zero chars) is rejected
#     by the length guard while Rust parses it as 0. Removing the strip
#     loop alone drifts on every zero-padded value over 10 characters.
#
#   * The `<= 10` length guard stops a REAL wrap, not a hypothetical one.
#     Values just above a multiple of 2^64 wrap into range: 2^64+5 =
#     18446744073709551621 evaluates to 5 in bash's 64-bit arithmetic, so
#     without the guard it would be ACCEPTED while coord rejects it --
#     the green-light-then-server-refuses class again.
#
# All three are pinned by the corpus (`#008`/`#09` for octal,
# `#00000000008` at 11 chars for the strip-before-guard ordering, and
# `#18446744073709551621` for the wrap). Do not delete one on the strength
# of a green suite without re-running the differential.
parses_as_i32() {
  local n="$1" digits sign=""
  [[ "$n" =~ ^[+-]?[0-9]+$ ]] || return 1
  digits="$n"
  case "$n" in
    -*) sign="-"; digits="${n#-}" ;;
    +*) digits="${n#+}" ;;
  esac
  while [[ "${digits:0:1}" == "0" && ${#digits} -gt 1 ]]; do
    digits="${digits:1}"
  done
  (( ${#digits} <= 10 )) || return 1
  if [[ "$sign" == "-" ]]; then
    (( 10#$digits <= 2147483648 ))
  else
    (( 10#$digits <= 2147483647 ))
  fi
}

validate_label() {
  local label="$1"
  if [[ "${label:0:6}" != "coord:" ]]; then
    echo "error: label must start with \"coord:\"" >&2
    return 1
  fi
  local rest="${label:6}"

  # Reject coord-set labels
  case "$rest" in
    state=*|blocked-by=*|specialist-decision=*)
      echo "error: coord-set label \"$label\" cannot be authored via skill" >&2
      return 1
      ;;
  esac

  # Retired hold-labels — rejected with guidance (mirrors coord's
  # RETIRED_HOLD_LABEL_ERR; retired 2026-06-20, nothing consumes the rows).
  case "$rest" in
    operator-review|version-bump|version-bump=*)
      echo "error: $label: retired label — labels no longer hold PRs; convert the PR to draft, or register a coord gate with a MergePr continuation" >&2
      return 1
      ;;
  esac

  # The merge-train priority lane. Rejected here with the working
  # alternative named — mirrors coord's `PRIORITY_LABEL_ERR`. This arm ALSO
  # catches the parameterised form (`coord:priority=1`), which must never be
  # accepted: the lever is ONE BIT, and an author writing `=1` is reaching for
  # numeric levels that do not exist. Before this arm existed the bare flag
  # fell through to the generic `parameterised labels need "=value"` while
  # `priority=1` reported `unknown coord:* label key` — two different
  # unhelpful errors for one cause, and neither naming the fix.
  case "$rest" in
    priority|priority=*)
      echo "error: coord:priority must be set on the PR itself (\`gh pr edit --add-label coord:priority\`) — a skill-set row is invisible on GitHub and inert in the merge scheduler, which only honours source='github'" >&2
      return 1
      ;;
  esac

  # Flag labels (no =). Accepted because live consumers read the rows
  # (dequeue-time merge-class routing; Tier-7 credibility gate).
  #
  # `blocked` / `experimental` / `credibility-override` are
  # RESTRICTIVE-or-inert — they downgrade routing, or relax a credibility
  # threshold inside a gate that still runs. `migrate-repair` is the ODD ONE
  # OUT, and the asymmetry is deliberate: it is the only flag here that
  # RELEASES a hold, i.e. can make a land happen that otherwise would not.
  # coord bounds it at the CONSUMING end rather than here — the validator
  # only decides whether the label may be SET, and
  # `merge_scheduler::migrate_self_blocking` independently refuses to honour
  # it unless the land is genuinely self-blocking. Setting it is cheap and
  # auditable; acting on it is not, and coord keeps those two decisions
  # separate. (Value mirrored from `MIGRATE_REPAIR_LABEL_SUFFIX`.)
  if [[ "$rest" == "blocked" || "$rest" == "experimental" || "$rest" == "credibility-override" || "$rest" == "migrate-repair" ]]; then
    return 0
  fi

  # Parameterised labels — must have key=value
  if [[ "$rest" != *=* ]]; then
    echo "error: parameterised labels need \"=value\"" >&2
    return 1
  fi
  local key="${rest%%=*}"
  local value="${rest#*=}"
  if [[ -z "$value" ]]; then
    echo "error: value after \"=\" cannot be empty" >&2
    return 1
  fi

  case "$key" in
    upstream-of|downstream-of)
      if [[ "$value" != *#* ]]; then
        echo "error: $key: missing \"#<pr_number>\"" >&2
        return 1
      fi
      local repo_part="${value%%#*}"
      local n_part="${value#*#}"
      if [[ -z "$repo_part" ]]; then
        echo "error: $key: missing repo" >&2
        return 1
      fi
      if ! repo_segments_well_formed "$repo_part"; then
        echo "error: $key: empty owner or repo segment around \"/\"" >&2
        return 1
      fi
      if ! parses_as_i32 "$n_part"; then
        echo "error: $key: pr_number must be int" >&2
        return 1
      fi
      ;;
    stacked-on)
      # `#<n>` (same repo, back-compat) OR `[<owner>/]<repo>#<n>` —
      # an empty repo part is the same-repo form.
      if [[ "$value" != *#* ]]; then
        echo "error: stacked-on: missing \"#<pr_number>\"" >&2
        return 1
      fi
      local repo_part="${value%%#*}"
      # An EMPTY repo part is the legitimate same-repo form (`=#<n>`); only a
      # non-empty one has segments to check.
      if [[ -n "$repo_part" ]] && ! repo_segments_well_formed "$repo_part"; then
        echo "error: stacked-on: empty owner or repo segment around \"/\"" >&2
        return 1
      fi
      local n_part="${value#*#}"
      if ! parses_as_i32 "$n_part"; then
        echo "error: stacked-on: pr_number must be int" >&2
        return 1
      fi
      ;;
    requires-tag)
      : # any non-empty value
      ;;
    merge-strategy)
      case "$value" in
        squash|rebase|merge) : ;;
        *) echo "error: merge-strategy: must be one of squash|rebase|merge" >&2; return 1 ;;
      esac
      ;;
    *)
      echo "error: unknown coord:* label key \"$key\"" >&2
      return 1
      ;;
  esac
  return 0
}

if ! validate_label "$LABEL"; then
  exit 2
fi

# ----- GitHub's label-name ceiling (deliberately NOT part of the mirror) -----
# GitHub caps a label NAME at 50 characters. coord has no such rule and should
# not grow one: `coord.pr_labels` stores a text column and the cap belongs to
# the GitHub API, not to the namespace. So this check lives OUTSIDE
# validate_label above, which mirrors labels_routes.rs::validate_label -- do
# not fold it in, or the next sync against coord will delete it as "not in
# coord".
#
# Without this pre-flight the caller gets GitHub's own mis-signposted pair:
#   gh label create        -> HTTP 422 ... name is too long (maximum is 50 characters)
#   gh pr edit --add-label -> '<label>' not found
# and the second one reads as a MISSING-label problem, sending the caller off
# to create a label that cannot exist.
#
# With the 8-character owner `qontinui` and a 4-digit PR number, the FULL
# `owner/repo#n` form overflows once the repo name reaches 17 characters
# (`downstream-of`), 19 (`upstream-of`) or 20 (`stacked-on`). The owner-dropped
# SHORT form always fits: it is 25 + name characters, and the longest repo name
# in the org is 23. Stated as a rule because a list of overflowing repo names
# goes stale on every rename -- #297's list already missed two. See SKILL.md,
# "GitHub caps a label name at 50 characters".

GH_LABEL_MAX=50

# Owner-dropped short form of a dep label: `<owner>/<repo>#<n>` -> `<repo>#<n>`,
# which coord canonicalizes back via `coord.tenant_repos` -- the grammar's own
# owner-optional arm, not a workaround.
#
# Prints NOTHING unless the short form is (a) a label this script would itself
# accept and (b) still the same repo. A suggestion the validator rejects, or one
# that silently retargets the edge at a different owner's repo, is worse than no
# suggestion at all -- the caller is being told to trust it. So the owner must
# match the owner of --repo (coord canonicalizes a bare name to the TENANT's
# owner, which is a round trip only for our own owner), and the candidate is run
# through the real validate_label rather than eyeballed.
#
# The owner proxy is deliberately CONSERVATIVE: it can only ever withhold a
# suggestion, never emit a retargeting one. It also withholds in three benign
# cases -- a --repo with no owner, an owner differing only in case (GitHub is
# case-insensitive here, this comparison is not), and a tenant owning repos
# under a second org. That is degradation, not a wrong answer; do not "fix" it
# by loosening the match.
#
# The validate_label round-trip is a BACKSTOP, not a reachable branch: given the
# guards above, `short` is valid by construction. It is kept so that editing
# those guards cannot silently start emitting a label the validator rejects.
# The self-test therefore does not assert it independently, and cannot.
#
# $1 = the label, $2 = the owner to expect (from --repo).
short_form() {
  local label="$1" expect_owner="$2"
  local rest="${label#coord:}"
  case "$rest" in
    upstream-of=*|downstream-of=*|stacked-on=*) : ;;
    *) return 0 ;;
  esac
  local key="${rest%%=*}"
  local value="${rest#*=}"
  [[ "$value" == *#* ]] || return 0
  local repo_part="${value%%#*}"
  local n_part="${value#*#}"
  [[ -n "$expect_owner" && "$repo_part" == "$expect_owner"/* ]] || return 0
  local bare="${repo_part#*/}"
  # a plain repo name: non-empty, and not itself a path
  [[ -n "$bare" && "$bare" != */* ]] || return 0
  local short="coord:$key=$bare#$n_part"
  validate_label "$short" >/dev/null 2>&1 || return 0
  printf '%s' "$short"
}

if (( ${#LABEL} > GH_LABEL_MAX )); then
  echo "error: label is ${#LABEL} characters; GitHub caps a label name at $GH_LABEL_MAX" >&2
  echo "       \"$LABEL\"" >&2
  SHORT="$(short_form "$LABEL" "${REPO%%/*}")"
  if [[ -n "$SHORT" && ${#SHORT} -le $GH_LABEL_MAX ]]; then
    echo "       drop the owner -- coord restores it via coord.tenant_repos:" >&2
    echo "         --label \"$SHORT\"   (${#SHORT} chars)" >&2
  else
    echo "       shorten the value; see SKILL.md, \"GitHub caps a label name at 50 characters\"" >&2
  fi
  echo "       NOTE: gh reports this as \"'<label>' not found\", which is NOT a" >&2
  echo "       missing-label problem -- gh label create cannot succeed either." >&2
  exit 2
fi

if [[ "$DRY_RUN" == "1" ]]; then
  echo "ok: label \"$LABEL\" is valid (${#LABEL}/$GH_LABEL_MAX chars) -- dry run, nothing sent"
  exit 0
fi

if [[ -z "${QONTINUI_AGENT_ID:-}" ]]; then
  echo "error: QONTINUI_AGENT_ID env var unset — coord-side ingest needs it" >&2
  exit 2
fi

# ----- step 1: gh-side label add ---------------------------------------------

if ! command -v gh >/dev/null 2>&1; then
  echo "error: gh CLI not on PATH — install + auth before running this skill" >&2
  exit 3
fi

echo "step 1/2: gh pr edit $REPO #$PR --add-label \"$LABEL\""
if ! gh pr edit "$PR" --repo "$REPO" --add-label "$LABEL"; then
  echo "error: gh pr edit failed" >&2
  exit 3
fi
echo "ok: gh added label \"$LABEL\" to $REPO#$PR"

# ----- step 2: coord-side ingest hook ----------------------------------------

PAYLOAD=$(python3 -c "
import json, sys
print(json.dumps({
    'agent_id': sys.argv[1],
    'repo': sys.argv[2],
    'pr_number': int(sys.argv[3]),
    'labels': [sys.argv[4]],
}))
" "$QONTINUI_AGENT_ID" "$REPO" "$PR" "$LABEL")

echo "step 2/2: POST $COORD_URL/pr-merge/labels"
# -w appends the HTTP code on its own line; plain -sS exits 0 on HTTP 4xx/5xx,
# which is how a 422 tenant_resolution_failed once printed as "ok, written=0".
RAW=$(curl -sS -w '\n%{http_code}' -X POST "$COORD_URL/pr-merge/labels" \
  -H "Content-Type: application/json" \
  -d "$PAYLOAD") || {
    echo "error: POST $COORD_URL/pr-merge/labels failed (coord unreachable?)" >&2
    echo "       gh-side label add succeeded; reconciler will eventually pick it up." >&2
    exit 4
}
HTTP_CODE=${RAW##*$'\n'}
RESPONSE=${RAW%$'\n'*}

if [[ "$HTTP_CODE" != 2* ]]; then
  echo "error: coord ingest returned HTTP $HTTP_CODE — body: $RESPONSE" >&2
  if [[ "$RESPONSE" == *tenant_resolution_failed* ]]; then
    echo "       QONTINUI_AGENT_ID must be an agent id coord knows (an agent_worktrees" >&2
    echo "       row, e.g. an ~/.qontinui/agent-runs/<uuid> id) — a session id or gate" >&2
    echo "       registered_by id does NOT resolve to a tenant." >&2
  fi
  echo "       gh-side label add succeeded (canonical); coord.pr_labels is out of sync" >&2
  echo "       until the reconciler ingests the GitHub label event." >&2
  exit 4
fi

# Parse the response — `written` should be 1, `rejected` should be empty.
if command -v python3 >/dev/null 2>&1; then
  TENANT_ID=$(echo "$RESPONSE" | python3 -c "import json, sys; d = json.load(sys.stdin); print(d.get('tenant_id', '?'))" 2>/dev/null || echo "?")
  WRITTEN=$(echo "$RESPONSE" | python3 -c "import json, sys; d = json.load(sys.stdin); print(d.get('written', 0))" 2>/dev/null || echo "0")
  REJECTED=$(echo "$RESPONSE" | python3 -c "import json, sys; d = json.load(sys.stdin); print(len(d.get('rejected', [])))" 2>/dev/null || echo "0")
else
  TENANT_ID="?"
  WRITTEN="?"
  REJECTED="?"
fi

if [[ "$REJECTED" != "0" ]]; then
  echo "error: coord rejected the label — body: $RESPONSE" >&2
  exit 4
fi

if [[ "$WRITTEN" == "0" ]]; then
  echo "error: coord wrote no pr_labels row (written=0, nothing rejected?) — body: $RESPONSE" >&2
  exit 4
fi

echo "ok: coord recorded label \"$LABEL\" in pr_labels (tenant_id=$TENANT_ID, written=$WRITTEN)"
