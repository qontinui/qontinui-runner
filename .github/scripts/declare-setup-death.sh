#!/usr/bin/env bash
# Declare a CI job's setup death "not a verdict on this PR" -- but ONLY when
# the PR itself cannot have caused it.
#
# WHY (plan 2026-11-17-runner-clippy-setup-death-guards-frontend-steps;
# dossier repair-classifier-misattribution). qontinui-coord's repair
# classifier (author_feedback.rs is_no_verdict_annotation) reads an annotation
# whose title contains `(not a verdict on this PR)` as "this red is harness,
# re-run it" instead of blaming the PR. Several setup steps read the PR's own
# tree or labels (pnpm/node setup, install, build, the sibling resolver), so
# an outcome-only declarer excused a PR for its own breakage. This reporter
# keeps the excuse for genuine harness deaths and withholds it whenever the
# PR could be the cause. Shape mirrors qontinui-web's declare-setup-death.sh
# (qontinui-web#2283), so any runner job can adopt it.
#
# INPUTS (env only; never interpolated into a shell string):
#   DECLARE_JOB        the job's display name; the declared title is
#                      "<DECLARE_JOB> SETUP FAILED (not a verdict on this PR)"
#   SETUP_STEPS        one row per setup step, in step order:
#                      `<name><TAB><conclusion><TAB><class>`; class is
#                        tree-free       reads nothing the PR controls
#                        frontend        reads the PR's frontend tree
#                        sibling-labels  reads the PR's coord dep labels
#                      a missing or unknown class is treated as `frontend`
#                      (the strictest diff rule -- fails closed)
#   VERDICT_STEPS      `<name><TAB><conclusion>` rows of the job's verdict
#                      step(s); any failure means the red IS a verdict
#   HARNESS_PATHS_RE   ERE over changed paths: the job's own harness. A PR
#                      touching any of it may have caused ANY setup death.
#                      This script's own path is always harness.
#   FRONTEND_INERT_RE  optional ERE: paths the frontend steps provably never
#                      read (an allow-list; default below)
#   SIBLING_LABEL_RE   optional ERE over PR label names that steer the
#                      sibling resolver (default below)
#   GITHUB_EVENT_NAME / GITHUB_SHA / GITHUB_WORKSPACE / GITHUB_EVENT_PATH
#   GIT_BOUND          optional seconds bound for each local git call (30);
#                      the one network fetch is bounded at twice that.
#
# DECISION (first failed setup step = F):
#   any verdict step failed, or no F            -> emit nothing
#   a setup or verdict row has no conclusion    -> withhold (UNKNOWN)
#   DECLARE_JOB missing or carries a reserved   -> withhold
#   phrase
#   PR diff unreadable / empty / not a PR merge -> withhold (UNKNOWN)
#   diff touches HARNESS_PATHS_RE               -> withhold
#   F is `frontend` and a changed path is not   -> withhold
#   in FRONTEND_INERT_RE
#   F is `sibling-labels` and the event payload -> withhold
#   is unreadable or names a SIBLING_LABEL_RE
#   label
#   else                                        -> declare (re-run once)
#
# A withhold is a ::notice:: that never carries the phrase coord matches.
# EXIT: always 0. A reporter never decides a check.
set -uo pipefail
trap 'exit 0' EXIT

export GIT_TERMINAL_PROMPT=0 GIT_NO_LAZY_FETCH=1

SELF_RE='^\.github/scripts/declare-setup-death\.sh$'
DEFAULT_FRONTEND_INERT_RE='^(src-tauri/|crates/|docs/|\.github/|Cargo\.(toml|lock)$|[^/]+\.md$)'
DEFAULT_SIBLING_LABEL_RE='^coord:(downstream|upstream)-of'
GIT_BOUND="${GIT_BOUND:-30}"
FETCH_BOUND=$((GIT_BOUND * 2))

# Workflow-command escaping (actions/toolkit command.ts).
esc_data() {
  local s="$1"
  s="${s//%/%25}"; s="${s//$'\r'/%0D}"; s="${s//$'\n'/%0A}"
  printf '%s' "$s"
}
esc_prop() {
  local s
  s="$(esc_data "$1")"
  s="${s//:/%3A}"; s="${s//,/%2C}"
  printf '%s' "$s"
}

JOB="${DECLARE_JOB:-}"
withhold() {
  echo "::notice title=$(esc_prop "${JOB:-setup death} setup death left as a verdict")::$(esc_data "$1")"
}

# Split a TAB row into fields WITHOUT `IFS=$'\t' read`: tab is IFS
# whitespace, so read merges adjacent tabs and an EMPTY middle field (a step
# whose conclusion rendered empty) would shift the next field into its place.
F1="" F2="" F3=""
split_row() {
  local line="$1" rest
  F1="${line%%$'\t'*}"; F2=""; F3=""
  [ "$F1" = "$line" ] && return
  rest="${line#*$'\t'}"
  F2="${rest%%$'\t'*}"
  [ "$F2" = "$rest" ] && return
  F3="${rest#*$'\t'}"
}

# --- verdict steps ----------------------------------------------------------
unreadable=""
while IFS= read -r line || [ -n "${line:-}" ]; do
  split_row "$line"; name="$F1" concl="$F2"
  [ -z "$name" ] && continue
  if [ "${concl:-}" = "failure" ]; then
    exit 0
  fi
  [ -z "${concl:-}" ] && unreadable="${unreadable:-$name}"
done <<< "${VERDICT_STEPS:-}"

# --- setup steps ------------------------------------------------------------
first="" first_class=""
while IFS= read -r line || [ -n "${line:-}" ]; do
  split_row "$line"; name="$F1" concl="$F2" class="$F3"
  [ -z "$name" ] && continue
  if [ -z "${concl:-}" ]; then
    unreadable="${unreadable:-$name}"
    continue
  fi
  if [ -z "$first" ] && [ "$concl" = "failure" ]; then
    first="$name"
    first_class="${class:-frontend}"
  fi
done <<< "${SETUP_STEPS:-}"

if [ -z "$first" ]; then
  if [ -n "$unreadable" ]; then
    withhold "step '${unreadable}' rendered no conclusion (a renamed step id renders empty), so whether a setup step failed is UNKNOWN."
  fi
  exit 0
fi
if [ -n "$unreadable" ]; then
  withhold "setup step '${first}' failed, but step '${unreadable}' rendered no conclusion, so the step list is UNKNOWN. This red is left to read as a verdict on this change."
  exit 0
fi
case "$first_class" in
  tree-free | frontend | sibling-labels) ;;
  *) first_class="frontend" ;;
esac

if [ -z "$JOB" ]; then
  withhold "setup step '${first}' failed, but DECLARE_JOB is unset, so no title can be declared."
  exit 0
fi
case "$JOB" in
  *"(not a verdict"* | *"OUT OF MEMORY"* | *"killed IN THE BUILD"* | *"TIMED OUT"*)
    withhold "setup step '${first}' failed, but DECLARE_JOB carries a phrase coord's classifier reserves, so no title is declared."
    exit 0
    ;;
esac

# --- the guard: may THIS PR have caused it? -----------------------------------
GUARD="UNKNOWN"
REASON=""
read_guard() {
  if [ "${GITHUB_EVENT_NAME:-}" != "pull_request" ]; then
    REASON="the event is '${GITHUB_EVENT_NAME:-unset}', not pull_request, so there is no PR diff to read"
    return
  fi
  local ws="${GITHUB_WORKSPACE:-.}" merge="${GITHUB_SHA:-}"
  if [ -z "$merge" ]; then
    REASON="GITHUB_SHA is unset"
    return
  fi
  # Parents come from the raw commit object, NOT `rev-list --parents` or
  # `$sha^1`: actions/checkout's default depth-1 fetch grafts the merge
  # commit, and a grafted commit reports no parents through the walker.
  local object
  object="$(timeout -k 5 "$GIT_BOUND" git -C "$ws" cat-file commit "$merge" 2> /dev/null)" || {
    REASON="GITHUB_SHA ${merge} is not readable in the checkout"
    return
  }
  local -a parents
  mapfile -t parents < <(sed -n '/^$/q; s/^parent //p' <<< "$object")
  if [ "${#parents[@]}" -ne 2 ]; then
    REASON="GITHUB_SHA ${merge} has ${#parents[@]} parent(s), not 2, so it is not a PR merge commit and its first parent is not the base"
    return
  fi
  local base="${parents[0]}" rc
  if ! timeout -k 5 "$GIT_BOUND" git -C "$ws" cat-file -e "${base}^{commit}" 2> /dev/null; then
    timeout -k 5 "$FETCH_BOUND" git -C "$ws" fetch --quiet --no-tags --depth=1 origin "$base" > /dev/null 2>&1
    rc=$?
    if [ "$rc" -ne 0 ]; then
      REASON="the merge commit's first parent ${base} could not be fetched (git fetch exited ${rc})"
      return
    fi
  fi
  local changed
  changed="$(timeout -k 5 "$GIT_BOUND" git -C "$ws" diff -z --no-renames --name-only "$base" "$merge" 2> /dev/null | tr '\0' '\n')"
  rc=${PIPESTATUS[0]}
  if [ "$rc" -ne 0 ]; then
    REASON="git diff ${base} ${merge} exited ${rc}, so this PR's diff is unread"
    return
  fi
  if [ -z "$changed" ]; then
    REASON="this PR's diff against its base is empty"
    return
  fi
  local hit
  hit="$(grep -E -m1 -e "$SELF_RE" ${HARNESS_PATHS_RE:+-e "$HARNESS_PATHS_RE"} <<< "$changed")"
  if [ -n "$hit" ]; then
    GUARD="true"
    REASON="this PR changes '${hit}', part of this job's own harness, so it can have caused the setup death"
    return
  fi
  case "$first_class" in
    frontend)
      hit="$(grep -E -v -m1 -e "${FRONTEND_INERT_RE:-$DEFAULT_FRONTEND_INERT_RE}" <<< "$changed")"
      if [ -n "$hit" ]; then
        GUARD="true"
        REASON="'${first}' reads this PR's frontend tree and the PR changes '${hit}', so the failure can be this PR's own"
        return
      fi
      ;;
    sibling-labels)
      local ev="${GITHUB_EVENT_PATH:-}" labels
      if [ -z "$ev" ] || [ ! -r "$ev" ] || ! command -v jq > /dev/null 2>&1; then
        REASON="'${first}' reads this PR's labels and the event payload is unreadable"
        return
      fi
      labels="$(jq -er '.pull_request.labels // empty | map(.name) | .[]?' "$ev" 2> /dev/null)"
      rc=$?
      # jq -e exits 4 on an empty label list, which is a readable answer.
      if [ "$rc" -ne 0 ] && [ "$rc" -ne 4 ]; then
        REASON="'${first}' reads this PR's labels and the event payload's labels could not be parsed (jq exited ${rc})"
        return
      fi
      hit="$(grep -E -m1 -e "${SIBLING_LABEL_RE:-$DEFAULT_SIBLING_LABEL_RE}" <<< "$labels")"
      if [ -n "$hit" ]; then
        GUARD="true"
        REASON="'${first}' resolves siblings from this PR's labels and the PR carries '${hit}', so the failure can be this PR's own"
        return
      fi
      ;;
  esac
  GUARD="false"
}
read_guard

if [ "$GUARD" = "false" ]; then
  echo "::error title=$(esc_prop "${JOB} SETUP FAILED (not a verdict on this PR)")::$(esc_data "step '${first}' failed before the verdict step ran and this PR cannot have caused it: re-run once")"
else
  withhold "setup step '${first}' failed, but the guard read ${GUARD}: ${REASON}. This red is left to read as a verdict on this change."
fi
exit 0
