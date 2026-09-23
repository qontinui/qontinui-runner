#!/usr/bin/env bash
# coord-pr-label — set a coord:* label on a PR via gh + record in coord.pr_labels.
#
# Phase 2 D2.6 of the PR Merge Orchestrator
# (qontinui-dev-notes/plans/2026-05-21-pr-merge-orchestrator-design.md).
#
# Validates the label against the `coord:*` namespace, adds the label over
# the REST issues-labels route (`gh api -X POST repos/<o>/<r>/issues/<n>/labels`
# -- NOT `gh pr edit --add-label`, whose GraphQL prefetch still selects the
# retired `repository.pullRequest.projectCards` field and exits 1 before
# touching the label on gh 2.46.0, measured 2026-09-03), then POSTs the same label to
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
                     coord, and QONTINUI_AGENT_ID is not required. It therefore
                     cannot check whether the label EXISTS -- that needs a send
                     -- and the report says so rather than leaving "is valid" to
                     imply it.

Required env:
  QONTINUI_AGENT_ID  — the spawning agent's UUID. Set by the agent-spawn
                       flow; if absent the skill exits with an error.

Optional env:
  COORD_URL          — coord base URL (then COORD_HTTP_URL). Default
                       https://coord.qontinui.io.

Examples:
  set-label.sh --repo qontinui/qontinui-coord --pr 75 \
      --label "coord:upstream-of=qontinui/qontinui-schemas#42"
  set-label.sh --repo qontinui/qontinui-coord --pr 75 --label coord:merge-strategy=squash

No label holds a PR. To hold one, convert it to draft:
  gh pr ready --undo 75 --repo qontinui/qontinui-coord
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

# Coord is the hosted service; a localhost default silently posted every
# label ingest at a port nothing listens on and reported it as "coord
# unreachable" (measured 2026-09-02). Same precedence every other coord
# caller uses: COORD_URL, then COORD_HTTP_URL, then the hosted base.
COORD_URL="${COORD_URL:-${COORD_HTTP_URL:-https://coord.qontinui.io}}"

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

  # `blocked` / `experimental` are the same retirement. coord matches them in
  # `data/repo_branches.rs` `inert_hold_labels` and posts
  # `render_inert_hold_label_comment` telling the author they do NOT hold the
  # PR; their only live effect is downgrading dequeue routing (Contending, no
  # fast-land). An author reaching for them wants a hold, so refuse and name
  # the one that works. (Accepting them here was the 2026-07-10 incident's
  # shape: a PR "held" by `coord:blocked` auto-merged once green.)
  case "$rest" in
    blocked|experimental)
      echo "error: $label: retired hold label — coord treats it as inert and the PR still auto-merges once green. To hold a PR, convert it to draft: gh pr ready --undo <n> --repo <owner/repo>" >&2
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
  # (the Tier-7 credibility gate; the migrate self-blocking check).
  #
  # `credibility-override` is BOUNDED — it relaxes a credibility threshold
  # inside a gate that still runs. `migrate-repair` is the ODD ONE
  # OUT, and the asymmetry is deliberate: it is the only flag here that
  # RELEASES a hold, i.e. can make a land happen that otherwise would not.
  # coord bounds it at the CONSUMING end rather than here — the validator
  # only decides whether the label may be SET, and
  # `merge_scheduler::migrate_self_blocking` independently refuses to honour
  # it unless the land is genuinely self-blocking. Setting it is cheap and
  # auditable; acting on it is not, and coord keeps those two decisions
  # separate. (Value mirrored from `MIGRATE_REPAIR_LABEL_SUFFIX`.)
  if [[ "$rest" == "credibility-override" || "$rest" == "migrate-repair" ]]; then
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

# ----- what a dry run CANNOT check -------------------------------------------
# `'<label>' not found` has TWO causes (SKILL.md, "Failure modes"). The ceiling
# check above closes cause 2 -- over 50 characters, therefore uncreatable -- and
# the diagnosis after `gh pr edit` below closes cause 1. A dry run reaches
# NEITHER end of that pair: it sends nothing, so it cannot ask GitHub whether the
# label exists, and an unqualified "is valid" is exactly the reassurance that
# invites the caller to assume it did. The real run then contradicts it, which
# leaves a `--dry-run` user in the same place #318 found them -- one entry point
# over.
#
# It says NOT CHECKED and never "does not exist": a dry run has no evidence in
# either direction, and reporting a label somebody already created as absent
# would be a fresh mis-signpost rather than a fix.
#
# The `gh label create` line is printed only for an OPEN-VALUED KEY, and the arm
# is KEYED rather than inferred from the presence of `=`. That distinction is the
# whole correctness of the split:
#
#   * OPEN-valued -- `upstream-of` / `downstream-of` / `stacked-on` carry a PR
#     number and are unique to the pair they wire; `requires-tag` carries a
#     caller-chosen pattern. Nothing pre-creates those, so `not found` is the
#     expected first answer for a new one and the create command is the fix.
#   * CLOSED or valueless -- the flag labels, and `merge-strategy`, whose value
#     is one of exactly three strings (`squash|rebase|merge`). Those are
#     repo-wide labels somebody creates once, so pointing at a repo-wide mutation
#     for one signposts work nobody needs -- the same over-broad-advice failure
#     the post-gh arm below narrows its match to avoid.
#
# A `*=*` test looks equivalent and is not: it sweeps `merge-strategy` in with
# the dep labels and then justifies the advice with "unique to the PR pair",
# which is a claim about a key that label is not. Same `case` shape as
# `short_form` above, for the same reason -- the set of keys is the fact, not the
# punctuation.
dry_run_existence_note() {
  echo "note: NOT checked -- whether \"$LABEL\" exists as a label in $REPO."
  echo "      A dry run sends nothing, so it cannot ask. This is the one cause of"
  echo "      \"'<label>' not found\" the ceiling check above does not cover."
  case "${LABEL#coord:}" in
    upstream-of=*|downstream-of=*|stacked-on=*|requires-tag=*)
      echo "      This key is open-valued, so its labels are not created on demand --"
      echo "      a dep label's value is unique to the PR pair it wires. If nobody has"
      echo "      created this one, a real send fails until you run:"
      echo "        gh label create \"$LABEL\" --repo $REPO"
      ;;
  esac
}

if [[ "$DRY_RUN" == "1" ]]; then
  echo "ok: label \"$LABEL\" is valid (${#LABEL}/$GH_LABEL_MAX chars) -- dry run, nothing sent"
  dry_run_existence_note
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

# The REST issues/labels route, NOT `gh pr edit --add-label`. `gh pr edit`
# opens with a GraphQL query that still selects `projectCards`, and GitHub now
# answers that with `GraphQL: Projects (classic) is being deprecated ...
# (repository.pullRequest.projectCards)` -- non-zero exit, nothing applied,
# even for a label that exists (measured 2026-09-02, qontinui/qontinui-coord#1857,
# where the skill reported "gh pr edit failed" and the PR sat in coord's train
# with no dependency edge). The REST call carries no such query, and — unlike
# `gh pr edit` — does NOT create a missing dynamic-value label as a side
# effect; that is done explicitly below, on the 404 this route reports for one.
echo "step 1/3: gh api POST repos/$REPO/issues/$PR/labels \"$LABEL\""

# gh's stderr is CAPTURED rather than let straight through, so that a
# `'<label>' not found` can be answered here instead of left for the caller to
# mis-read. It is re-emitted verbatim first: gh's own wording is the evidence
# for anything added below it, and swallowing it would trade one bad diagnostic
# for another.
#
# `2>&1 1>&3` is order-sensitive. Inside `$( )` stdout is the capture pipe, so
# stderr is pointed at that pipe FIRST and stdout is then restored to the real
# one through the saved fd -- which is why gh's success output (the PR URL)
# still reaches the terminal. Writing `1>&3 2>&1` captures stdout and leaks
# stderr, i.e. exactly backwards.
#
# The fd is allocated by bash (`{gh_out}`) rather than hardcoded as 3: a literal
# `exec 3>&1` clobbers whatever the caller had on fd 3, and `exec 3>&-` then
# CLOSES it rather than restoring it. Nothing invokes this script that way
# today, but the automatic form costs nothing and cannot.
GH_RC=0
exec {gh_out}>&1
GH_STDERR="$(gh api --silent -X POST "repos/$REPO/issues/$PR/labels" -f "labels[]=$LABEL" 2>&1 1>&"$gh_out")" || GH_RC=$?
exec {gh_out}>&-

# Re-emitted on BOTH paths, before anything is decided about it. Capturing gh's
# stderr in order to answer ONE failure must not silently eat what it says the
# rest of the time: gh writes to stderr on SUCCESS too -- the
# `A new release of gh is available` notice, deprecation and auth-scope warnings
# -- and swallowing those would be this change committing the same offence it
# exists to fix, one path over.
if [[ -n "$GH_STDERR" ]]; then
  printf '%s\n' "$GH_STDERR" >&2
fi

if (( GH_RC != 0 )); then
  echo "error: label add failed (exit $GH_RC)" >&2

  # The REST route names this cause exactly: `Label does not exist` (HTTP 404).
  # By this line the label has already cleared the ceiling check above, so the
  # over-50-characters cause is RULED OUT and what is left is a dynamic-value
  # label nobody has created yet. Create it and retry ONCE. This overturns the
  # earlier "print the command, never create on demand" stance: a label is a
  # reversible, mechanical mutation (`gh label delete` undoes it), the
  # `coord:stacked-on=#<n>` namespace already carries one label per stacked PR
  # by design, and stopping an autonomous session on it was costing a
  # dependency edge every time [policy: do-reversible-mechanical-work].
  if [[ "$GH_STDERR" == *"Label does not exist"* ]]; then
    echo "       \"$LABEL\" does not exist in $REPO yet -- creating it (${#LABEL}/$GH_LABEL_MAX chars)" >&2
    if ! gh label create "$LABEL" --repo "$REPO" --color 0E8A16 \
         --description "coord merge-train label (set by coord-pr-label)"; then
      echo "error: gh label create failed for \"$LABEL\"" >&2
      exit 3
    fi
    GH_RC=0
    exec {gh_out}>&1
    GH_STDERR="$(gh api --silent -X POST "repos/$REPO/issues/$PR/labels" -f "labels[]=$LABEL" 2>&1 1>&"$gh_out")" || GH_RC=$?
    exec {gh_out}>&-
    if [[ -n "$GH_STDERR" ]]; then
      printf '%s\n' "$GH_STDERR" >&2
    fi
    if (( GH_RC != 0 )); then
      echo "error: label add failed after create (exit $GH_RC)" >&2
      exit 3
    fi
  else
    exit 3
  fi
fi
echo "ok: gh added label \"$LABEL\" to $REPO#$PR"

# ----- step 2: coord-side ingest hook ----------------------------------------
#
# WRONG-TENANT GUARD (coord finding for 2026-09-23; same class as
# qontinui-claude-config#1104 in handoff-stuck-pr.sh). `POST /pr-merge/labels`
# carries no credential: coord writes the row under the tenant of
# QONTINUI_AGENT_ID's `coord.agent_worktrees` row. On a device bound to several
# tenants that row is frequently stamped with the WRONG one -- an anonymous
# allocate that names no tenant resolves to the device's legacy pointer (coord
# `agent_worktrees::resolve_device_tenant`, P5a still `shadow`) -- and coord's
# own ownership check (`labels_routes::resolve_ingest_tenant`,
# COORD_LABEL_INGEST_OWNERSHIP_MODE) defaults to `shadow`, which meters the
# disagreement and writes under the inherited tenant anyway. Measured
# 2026-09-23: two `coord:stacked-on=` rows written under meryts-2-0 (b3ecb579)
# for qontinui/* PRs, and every cross-repo `upstream-of` / `downstream-of`
# refused as "not registered to this tenant".
#
# So before the real write, this step PROVES the write tenant owns $REPO:
#   2a. a probe -- the same POST with `labels: []` in `merge` mode, which writes
#       and deletes no pr_labels row (it does re-run coord's dependency-edge
#       resync for the PR, which may prune an edge that is already stale) -- reads back the tenant coord
#       would write under (the response's `tenant_id`);
#   2b. a device credential FOR that tenant (a static token whose `tenant_id`
#       claim is that tenant, else POST /agents/credential naming it; a minted
#       token claiming any other tenant is rejected, never used) asks
#       `GET /pr-merge/<owner%2Fname>/<pr>/author-session`, which answers 200
#       only when the caller's tenant owns the repo and 404 otherwise (coord
#       `pr_merge::get_author_session`). gh has just proven the PR exists, so a
#       404 REFUTES ownership.
# Proven -> the real POST. Refuted, or anything that is not a proof (no device
# id, no credential for that tenant, a 401/5xx/transport failure) -> the coord
# row is WITHHELD and the script exits 5 saying which. That is the fail-closed
# direction: the GitHub label is the canonical copy, it is already applied, and
# coord's merge ordering reads the dependency edge from it; a wrong-tenant row
# is what does harm. A probe answered with a well-formed body (`written` and
# `rejected` present) whose `tenant_id` KEY is absent means coord's `enforce`
# arm re-tenanted the write itself (the field is omitted exactly then), so
# coord made the ownership decision and 2b is skipped. Any other body -- empty,
# non-JSON, a null tenant_id -- is UNKNOWN and withholds.

TMPD="$(mktemp -d)" || { echo "error: mktemp -d failed" >&2; exit 4; }
trap 'rm -rf "$TMPD"' EXIT
HTTP_CONNECT_TIMEOUT="${COORD_PR_LABEL_CONNECT_TIMEOUT:-5}"
HTTP_TIMEOUT="${COORD_PR_LABEL_HTTP_TIMEOUT:-20}"
RC_WITHHELD=5

# A native curl opens -o itself; on an MSYS box the POSIX path must cross as a
# Windows one -- the same helper handoff-stuck-pr.sh / coord-revive use.
curl_path() { if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s' "$1"; fi; }

is_uuid() {
  local re='^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$'
  [[ "$1" =~ $re ]]
}

# jwt_claim <jwt> <claim> -> the payload claim as a string, or nothing. Reads the
# PAYLOAD only (base64url, not encrypted) and never prints the token.
jwt_claim() {
  H_JWT="$1" H_CLAIM="$2" python3 - <<'PY' 2>/dev/null || true
import base64, json, os
try:
    seg = os.environ["H_JWT"].split(".")[1]
    seg += "=" * (-len(seg) % 4)
    v = json.loads(base64.urlsafe_b64decode(seg)).get(os.environ["H_CLAIM"])  # envelope-ok: a JWT claim, not a fleet response envelope
    print(v if isinstance(v, (str, int, float)) and not isinstance(v, bool) else "")
except Exception:
    print("")
PY
}

jwt_shaped() {
  case "$1" in "" | *[!A-Za-z0-9._-]* ) return 1 ;; esac
  [ "$(printf '%s' "$1" | tr -cd '.' | wc -c | tr -d '[:space:]')" = "2" ]
}

# jwt_usable_for <jwt> <tenant> -> 0 iff JWT-shaped, exp >= 60 s away, and its
# `tenant_id` claim IS <tenant> (case-folded: coord's claims are lowercase).
jwt_usable_for() {
  local exp claim now
  jwt_shaped "$1" || return 1
  exp="$(jwt_claim "$1" exp)"; exp="${exp%%.*}"
  [[ "$exp" =~ ^[0-9]+$ ]] || return 1
  now="$(date +%s)"
  (( exp - now > 60 )) || return 1
  claim="$(jwt_claim "$1" tenant_id)"
  [ "${claim,,}" = "${2,,}" ]
}

# device_id -> this box's coord device id ($QONTINUI_MACHINE_ID, else
# ~/.qontinui/machine.json `device_id` / `machine_id`), or nothing.
device_id() {
  local home_dir="${HOME:-${USERPROFILE:-}}" mf
  if [ -n "${QONTINUI_MACHINE_ID:-}" ]; then printf '%s' "$QONTINUI_MACHINE_ID"; return 0; fi
  mf="$home_dir/.qontinui/machine.json"
  [ -n "$home_dir" ] && [ -r "$mf" ] || return 0
  python3 -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: sys.exit(0)
if not isinstance(d, dict): sys.exit(0)
v=d.get("device_id") or d.get("machine_id") or ""  # envelope-ok: machine.json is a local config file, not a fleet response
print(v if isinstance(v,str) else "")' < "$mf" 2>/dev/null | tr -d '[:space:]' || true
}

# stage_bearer_for <tenant> -> writes `Authorization: Bearer <jwt>` to
# $TMPD/bearer.hdr (0600, never on argv) and prints its source (env|file|mint),
# or prints `rejected(<why>)` and writes nothing. Only a token whose `tenant_id`
# claim IS <tenant> is ever staged: a token for another tenant would answer the
# ownership door about the WRONG tenant, which is the defect this guards.
stage_bearer_for() {
  local want="$1" home_dir="${HOME:-${USERPROFILE:-}}" jwt="" src="" why="" f dev code c
  rm -f "$TMPD/bearer.hdr"
  f="$(printf '%s' "${COORD_DEVICE_JWT:-}" | tr -d '[:space:]')"
  if [ -n "$f" ]; then
    if jwt_usable_for "$f" "$want"; then jwt="$f"; src=env
    else why="\$COORD_DEVICE_JWT is stale or claims another tenant"; fi
  fi
  if [ -z "$jwt" ] && [ -n "$home_dir" ] && [ -r "$home_dir/.qontinui/coord-device-jwt" ]; then
    f="$(tr -d '[:space:]' < "$home_dir/.qontinui/coord-device-jwt" 2>/dev/null || true)"
    if jwt_usable_for "$f" "$want"; then jwt="$f"; src=file
    else why="${why:+$why; }~/.qontinui/coord-device-jwt is stale or claims another tenant"; fi
  fi
  if [ -z "$jwt" ]; then
    dev="$(device_id)"
    if [ -z "$dev" ]; then
      why="${why:+$why; }no device_id (\$QONTINUI_MACHINE_ID / ~/.qontinui/machine.json) to mint with"
    else
      ( umask 077; : > "$TMPD/mint.json" )
      code="$(curl -sS -o "$(curl_path "$TMPD/mint.json")" -w '%{http_code}' \
        --connect-timeout "$HTTP_CONNECT_TIMEOUT" -m "$HTTP_TIMEOUT" \
        -X POST "$COORD_URL/agents/credential" -H "Content-Type: application/json" \
        -d "$(H_DEV="$dev" H_TENANT="$want" python3 -c 'import json,os; print(json.dumps({"device_id":os.environ["H_DEV"],"tenant_id":os.environ["H_TENANT"]}))')" \
        2>/dev/null)" || code="000"
      if [ "$code" = 200 ]; then
        f="$(python3 -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: sys.exit(0)
if isinstance(d, dict):
    for k in ("token","agent_jwt","jwt","access_token"):  # envelope-ok: the spellings coord-revive L5 reads
        v=d.get(k)
        if isinstance(v,str) and v: print(v); break' < "$TMPD/mint.json" 2>/dev/null | tr -d '[:space:]' || true)"
        c="$(jwt_claim "$f" tenant_id)"
        if jwt_usable_for "$f" "$want"; then jwt="$f"; src=mint
        elif jwt_shaped "$f" && [ "${c,,}" = "${want,,}" ]; then why="${why:+$why; }POST /agents/credential for tenant $want returned a token that is expired or carries no exp -- not used"
        else why="${why:+$why; }POST /agents/credential for tenant $want returned a token claiming tenant ${c:-<none>} (a coord predating the tenant_id field mints for the device's legacy pointer) -- not used"; fi
      else
        why="${why:+$why; }POST /agents/credential for tenant $want answered HTTP $code"
      fi
      rm -f "$TMPD/mint.json"
    fi
  fi
  if [ -n "$jwt" ]; then
    ( umask 077; printf 'Authorization: Bearer %s\n' "$jwt" > "$TMPD/bearer.hdr" )
    printf '%s' "$src"
  else
    printf 'rejected(%s)' "${why:-no credential}"
  fi
}

# post_labels <labels-json-array> -> POSTs to /pr-merge/labels; sets POST_CODE
# (000 on a transport failure) and POST_BODY.
post_labels() {
  local payload
  payload="$(H_AGENT="$QONTINUI_AGENT_ID" H_REPO="$REPO" H_PR="$PR" H_LABELS="$1" python3 -c '
import json, os
print(json.dumps({
    "agent_id": os.environ["H_AGENT"],
    "repo": os.environ["H_REPO"],
    "pr_number": int(os.environ["H_PR"]),
    "labels": json.loads(os.environ["H_LABELS"]),
    "mode": "merge",
}))')"
  : > "$TMPD/post.json"
  POST_CODE="$(curl -sS -o "$(curl_path "$TMPD/post.json")" -w '%{http_code}' \
    --connect-timeout "$HTTP_CONNECT_TIMEOUT" -m "$HTTP_TIMEOUT" \
    -X POST "$COORD_URL/pr-merge/labels" -H "Content-Type: application/json" \
    -d "$payload" 2>/dev/null)" || POST_CODE="000"
  POST_CODE="${POST_CODE:-000}"
  POST_BODY="$(cat "$TMPD/post.json" 2>/dev/null || true)"
}

# json_field <field> -> reads $POST_BODY; prints the field, or nothing.
json_field() {
  printf '%s' "$POST_BODY" | H_F="$1" python3 -c 'import json,os,sys
try: d=json.load(sys.stdin)
except Exception: sys.exit(0)
v=d.get(os.environ["H_F"]) if isinstance(d, dict) else None  # envelope-ok: the /pr-merge/labels response body, read field by field
if isinstance(v, list): print(len(v))
elif v is not None: print(v)' 2>/dev/null || true
}

# probe_tenant -> reads $POST_BODY: `tenant:<value>` when the key is present
# and a string, `absent` when the body is a well-formed label-set response
# with NO tenant_id key (coord's enforce arm re-tenanted), `bad` otherwise.
probe_tenant() {
  printf '%s' "$POST_BODY" | python3 -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: print("bad"); sys.exit(0)
if not isinstance(d, dict) or "written" not in d or "rejected" not in d: print("bad"); sys.exit(0)  # envelope-ok: the /pr-merge/labels response body
if "tenant_id" not in d: print("absent"); sys.exit(0)
v=d["tenant_id"]
print("tenant:"+v if isinstance(v,str) and v else "bad")' 2>/dev/null || echo bad
}

# bearer_url_ok -> 0 iff $COORD_URL is https or loopback http: a device JWT is
# never sent in clear text to an arbitrary host.
bearer_url_ok() {
  case "$COORD_URL" in *@*) return 1 ;; esac  # userinfo would retarget the host
  case "$COORD_URL" in
    https://*|http://127.0.0.1|http://127.0.0.1:[0-9]*|http://127.0.0.1/*|http://localhost|http://localhost:[0-9]*|http://localhost/*|"http://[::1]"*) return 0 ;;
  esac
  return 1
}

# non_2xx_exit -> the shared report for a non-2xx /pr-merge/labels answer.
non_2xx_exit() {
  echo "error: coord ingest returned HTTP $POST_CODE — body: $POST_BODY" >&2
  if [[ "$POST_BODY" == *tenant_resolution_failed* ]]; then
    echo "       QONTINUI_AGENT_ID must be an agent id coord knows (an agent_worktrees" >&2
    echo "       row, e.g. an ~/.qontinui/agent-runs/<uuid> id) — a session id or gate" >&2
    echo "       registered_by id does NOT resolve to a tenant." >&2
  fi
  if [[ "$POST_BODY" == *repo_not_owned_by_tenant* ]]; then
    echo "       coord's own ownership check (enforce) refused: no single tenant this" >&2
    echo "       agent's device is bound to owns $REPO. No row was written." >&2
    echo "       gh-side label add succeeded (canonical); merge ordering reads it from GitHub." >&2
    exit "$RC_WITHHELD"
  fi
  echo "       gh-side label add succeeded (canonical); coord.pr_labels is out of sync" >&2
  echo "       until the reconciler ingests the GitHub label event." >&2
  exit 4
}

withhold() { # <why>
  echo "WITHHELD: coord.pr_labels row NOT written -- $1" >&2
  echo "       gh-side label add succeeded (canonical), and coord's merge ordering reads" >&2
  echo "       the dependency edge from the GitHub label, so nothing is lost by withholding." >&2
  echo "       To also record the coord_skill row, use a QONTINUI_AGENT_ID allocated under the" >&2
  echo "       tenant that owns $REPO (a wrong-tenant row is what this guard exists to prevent)." >&2
  exit "$RC_WITHHELD"
}

echo "step 2/3: probe the tenant coord would write under (POST $COORD_URL/pr-merge/labels, labels=[] -- writes no pr_labels row)"
post_labels '[]'
if [[ "$POST_CODE" == 000 ]]; then
  echo "error: POST $COORD_URL/pr-merge/labels failed (coord unreachable?)" >&2
  echo "       gh-side label add succeeded; reconciler will eventually pick it up." >&2
  exit 4
fi
[[ "$POST_CODE" == 2* ]] || non_2xx_exit
PROBE="$(probe_tenant)"
WRITE_TENANT=""
case "$PROBE" in
  tenant:*) WRITE_TENANT="${PROBE#tenant:}"; WRITE_TENANT="${WRITE_TENANT,,}" ;;
  absent) : ;;
  *) withhold "the probe answered HTTP $POST_CODE with a body that is not a label-set response ($(printf '%s' "$POST_BODY" | head -c 200)); the write tenant is UNKNOWN" ;;
esac
OWNER_NOTE=""
if [[ "$PROBE" == absent ]]; then
  OWNER_NOTE="coord derived the tenant from repo ownership itself (the probe echoed no tenant_id: COORD_LABEL_INGEST_OWNERSHIP_MODE=enforce re-tenanted it)"
elif ! is_uuid "$WRITE_TENANT"; then
  withhold "the probe answered tenant_id '$WRITE_TENANT', which is not a uuid; the write tenant is UNKNOWN"
elif ! bearer_url_ok; then
  withhold "ownership of $REPO by the write tenant $WRITE_TENANT is UNKNOWN: COORD_URL=$COORD_URL is neither https nor loopback, so no device credential is sent to it"
else
  BEARER_SRC="$(stage_bearer_for "$WRITE_TENANT")"
  case "$BEARER_SRC" in
    rejected*)
      withhold "ownership of $REPO by the write tenant $WRITE_TENANT is UNKNOWN: no credential for that tenant (${BEARER_SRC#rejected})" ;;
  esac
  ENC_REPO="${REPO//\//%2F}"
  : > "$TMPD/door.json"
  DOOR_CODE="$(curl -sS -o "$(curl_path "$TMPD/door.json")" -w '%{http_code}' \
    --connect-timeout "$HTTP_CONNECT_TIMEOUT" -m "$HTTP_TIMEOUT" \
    -H "@$(curl_path "$TMPD/bearer.hdr")" \
    "$COORD_URL/pr-merge/$ENC_REPO/$PR/author-session" 2>/dev/null)" || DOOR_CODE="000"
  rm -f "$TMPD/bearer.hdr"
  case "${DOOR_CODE:-000}" in
    200) OWNER_NOTE="proven: tenant $WRITE_TENANT owns $REPO (author-session door answered 200, bearer=$BEARER_SRC)" ;;
    404) withhold "the write tenant $WRITE_TENANT does NOT own $REPO as coord knows it (author-session door answered 404 under a token claiming it, bearer=$BEARER_SRC; coord answers the same 404 for a repo it has not registered). Most likely QONTINUI_AGENT_ID's worktree row carries the wrong tenant -- the multi-tenant-device defect" ;;
    *)   withhold "ownership of $REPO by the write tenant $WRITE_TENANT is UNKNOWN: the author-session door answered HTTP ${DOOR_CODE:-000} (bearer=$BEARER_SRC)" ;;
  esac
fi
echo "ok: owner check: $OWNER_NOTE"

echo "step 3/3: POST $COORD_URL/pr-merge/labels \"$LABEL\""
post_labels "$(H_L="$LABEL" python3 -c 'import json,os; print(json.dumps([os.environ["H_L"]]))')"
if [[ "$POST_CODE" == 000 ]]; then
  echo "error: POST $COORD_URL/pr-merge/labels failed (coord unreachable?)" >&2
  echo "       gh-side label add succeeded; reconciler will eventually pick it up." >&2
  exit 4
fi
[[ "$POST_CODE" == 2* ]] || non_2xx_exit

TENANT_ID="$(json_field tenant_id)"; TENANT_ID="${TENANT_ID:-?}"
WRITTEN="$(json_field written)"; WRITTEN="${WRITTEN:-0}"
REJECTED="$(json_field rejected)"; REJECTED="${REJECTED:-0}"

if [[ "$REJECTED" != "0" ]]; then
  echo "error: coord rejected the label — body: $POST_BODY" >&2
  exit 4
fi

if [[ "$WRITTEN" == "0" ]]; then
  echo "error: coord wrote no pr_labels row (written=0, nothing rejected?) — body: $POST_BODY" >&2
  exit 4
fi

# The write must land where the probe said it would. A different tenant here
# means the agent row changed between the two calls; say so rather than print
# an unqualified ok.
if [[ -n "$WRITE_TENANT" && "$TENANT_ID" != "?" && "${TENANT_ID,,}" != "$WRITE_TENANT" ]]; then
  echo "error: coord wrote under tenant $TENANT_ID, not the proven $WRITE_TENANT — body: $POST_BODY" >&2
  echo "       A coord_skill row for \"$LABEL\" on $REPO#$PR now EXISTS under tenant $TENANT_ID" >&2
  echo "       (the agent row changed between probe and write). Removing it needs the" >&2
  echo "       admin-gated DELETE /pr-merge/labels/<owner>/<repo>/<pr>/<label>; report it." >&2
  exit 4
fi

echo "ok: coord recorded label \"$LABEL\" in pr_labels (tenant_id=$TENANT_ID, written=$WRITTEN)"
