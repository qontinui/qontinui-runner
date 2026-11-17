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
#                        sibling-labels  resolves siblings from coord dep
#                                        labels (ALWAYS withheld, see below)
#                      a missing or unknown class makes the step list
#                      UNKNOWN (withhold)
#   VERDICT_STEPS      `<name><TAB><conclusion>` rows of the job's verdict
#                      step(s); any failure means the red IS a verdict
#   HARNESS_PATHS_RE   REQUIRED ERE over changed paths: the job's own harness. A PR
#                      touching any of it may have caused ANY setup death.
#                      This script's own path is always harness.
#   FRONTEND_INERT_RE  optional ERE: paths no frontend step is known to read
#                      in a way that can FAIL it (an allow-list; default
#                      below). The vite build's Tailwind scan reads the whole
#                      tree for class candidates; malformed candidates were
#                      probed (tailwindcss 4.2.4, 2026-11-17) and did not fail
#                      the build -- observed, not proven.
#                      ASSUMPTION: vite.config.ts aliases @qontinui/schemas to
#                      the label-resolved sibling, and no src/ file imports it
#                      today. Once one does, a sibling-driven build death on an
#                      inert diff would be declared; withhold frontend deaths
#                      on a non-default sibling ref before adding that import.
#   GITHUB_EVENT_NAME / GITHUB_SHA / GITHUB_WORKSPACE
#   GIT_BOUND          optional positive-integer seconds bound for each local git call (30);
#                      the one network fetch is bounded at twice that.
#
# DECISION (first failed setup step = F):
#   DECLARE_JOB unset or carries a reserved      -> withhold (generic title)
#   phrase
#   a supplied regex does not compile            -> withhold (UNKNOWN)
#   no setup or no verdict rows; a row with no   -> withhold (UNKNOWN)
#   name, a conclusion outside success|failure|
#   cancelled|skipped, or (setup) no known class
#   any verdict step failed, or no F             -> emit nothing
#   F is `sibling-labels`                        -> withhold: the resolver
#     reads this PR's LIVE labels and reverse-searches open sibling PRs for
#     labels naming this PR (an ambiguous pairing hard-fails it); none of
#     that is in the event payload, so the PR may always be the cause
#   PR diff unreadable / empty / not a PR merge  -> withhold (UNKNOWN)
#   a grep over the diff errors                  -> withhold (UNKNOWN)
#   diff touches HARNESS_PATHS_RE                -> withhold
#   F is `frontend` and a changed path is not    -> withhold
#   in FRONTEND_INERT_RE
#   else                                         -> declare (re-run once)
#
# A withhold is a ::notice:: that never carries the phrase coord matches.
# EXIT: always 0. A reporter never decides a check.
set -uo pipefail
trap 'exit 0' EXIT

export GIT_TERMINAL_PROMPT=0 GIT_NO_LAZY_FETCH=1

SELF_RE='^\.github/scripts/declare-setup-death\.sh$'
DEFAULT_FRONTEND_INERT_RE='^(src-tauri/|crates/|docs/|\.github/|Cargo\.(toml|lock)$|[^/]+\.md$)'
INERT_RE="${FRONTEND_INERT_RE:-$DEFAULT_FRONTEND_INERT_RE}"
GIT_BOUND="${GIT_BOUND:-30}"

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

# The reserved-phrase check runs before ANY output, so no withhold title can
# carry a phrase coord's classifier matches.
JOB="${DECLARE_JOB:-}"
JOB_OK=1
case "$JOB" in
  "" | *"(not a verdict"* | *"OUT OF MEMORY"* | *"killed IN THE BUILD"* | *"TIMED OUT"*) JOB_OK=0 ;;
esac
withhold() {
  local t="setup death"
  [ "$JOB_OK" = 1 ] && t="$JOB setup death"
  echo "::notice title=$(esc_prop "$t left as a verdict")::$(esc_data "$1")"
}
if [ "$JOB_OK" = 0 ]; then
  withhold "DECLARE_JOB is unset or carries a phrase coord's classifier reserves, so no title can be declared."
  exit 0
fi

# Required inputs and bounds. An unset harness pattern would leave the job's
# own workflow unguarded; a non-numeric bound would abort silently.
if [ -z "${HARNESS_PATHS_RE:-}" ]; then
  withhold "HARNESS_PATHS_RE is unset, so this job's own harness is unread."
  exit 0
fi
if ! [[ "$GIT_BOUND" =~ ^[1-9][0-9]*$ ]]; then
  withhold "GIT_BOUND '${GIT_BOUND}' is not a positive integer, so no git call can be bounded."
  exit 0
fi
FETCH_BOUND=$((GIT_BOUND * 2))

# A regex that does not compile makes grep exit 2 with no output, which would
# read as "no match". Probe every supplied pattern first.
for re in "$SELF_RE" "$INERT_RE" "$HARNESS_PATHS_RE"; do
  grep -E -q -e "$re" < /dev/null 2> /dev/null
  if [ $? -gt 1 ]; then
    withhold "the pattern '${re}' is not a valid ERE, so the guard cannot be read. A setup death (if any) is left to read as a verdict."
    exit 0
  fi
done

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
valid_concl() {
  case "$1" in success | failure | cancelled | skipped) return 0 ;; *) return 1 ;; esac
}

# --- verdict steps ----------------------------------------------------------
unreadable="" vrows=0
while IFS= read -r line || [ -n "${line:-}" ]; do
  [ -z "$line" ] && continue
  vrows=$((vrows + 1))
  split_row "$line"; name="$F1" concl="$F2"
  if [ "$concl" = "failure" ]; then
    exit 0
  fi
  if [ -z "$name" ] || ! valid_concl "$concl"; then
    unreadable="${unreadable:-${name:-<unnamed>}}"
  fi
done <<< "${VERDICT_STEPS:-}"

# --- setup steps ------------------------------------------------------------
first="" first_class="" rows=0
while IFS= read -r line || [ -n "${line:-}" ]; do
  [ -z "$line" ] && continue
  rows=$((rows + 1))
  split_row "$line"; name="$F1" concl="$F2" class="$F3"
  case "$class" in tree-free | frontend | sibling-labels) ;; *) class="" ;; esac
  if [ -z "$name" ] || ! valid_concl "$concl" || [ -z "$class" ]; then
    unreadable="${unreadable:-${name:-<unnamed>}}"
    continue
  fi
  if [ -z "$first" ] && [ "$concl" = "failure" ]; then
    first="$name"
    first_class="$class"
  fi
done <<< "${SETUP_STEPS:-}"

if [ "$vrows" -eq 0 ]; then
  withhold "VERDICT_STEPS carries no rows, so whether the verdict step failed is UNKNOWN."
  exit 0
fi
if [ "$rows" -eq 0 ]; then
  withhold "SETUP_STEPS carries no rows, so whether a setup step failed is UNKNOWN."
  exit 0
fi
if [ -z "$first" ]; then
  if [ -n "$unreadable" ]; then
    withhold "step '${unreadable}' rendered no usable conclusion or class (a renamed step id renders empty), so whether a setup step failed is UNKNOWN."
  fi
  exit 0
fi
if [ -n "$unreadable" ]; then
  withhold "setup step '${first}' failed, but step '${unreadable}' rendered no usable conclusion or class, so the step list is UNKNOWN. This red is left to read as a verdict on this change."
  exit 0
fi

# --- the guard: may THIS PR have caused it? -----------------------------------
GUARD="UNKNOWN"
REASON=""
# grep_hit <invert:0|1> <re> : first (non-)matching changed path in HIT;
# returns 2 when grep itself failed.
HIT=""
grep_hit() {
  local rc
  if [ "$1" = 1 ]; then
    HIT="$(grep -E -v -m1 -e "$2" <<< "$CHANGED")"; rc=$?
  else
    HIT="$(grep -E -m1 -e "$2" <<< "$CHANGED")"; rc=$?
  fi
  [ "$rc" -gt 1 ] && return 2
  return 0
}
CHANGED=""
read_guard() {
  if [ "$first_class" = "sibling-labels" ]; then
    GUARD="true"
    REASON="'${first}' resolves siblings from this PR's live labels and from open sibling PRs' labels that name this PR, none of which this reporter can read, so the failure can be this PR's own pairing"
    return
  fi
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
  # With pipefail, the substitution's status is git's when git fails.
  # NUL-delimited paths become lines; a newline INSIDE a path is first
  # mapped to \001 so it cannot split one path into several inert ones.
  CHANGED="$(timeout -k 5 "$GIT_BOUND" git -C "$ws" diff -z --no-renames --name-only "$base" "$merge" 2> /dev/null | tr '\n\0' '\001\n')"
  rc=$?
  if [ "$rc" -ne 0 ]; then
    REASON="git diff ${base} ${merge} exited ${rc}, so this PR's diff is unread"
    return
  fi
  if [[ "$CHANGED" == *$'\001'* ]]; then
    GUARD="true"
    REASON="this PR changes a path containing a newline, which the path guards cannot classify"
    return
  fi
  if [ -z "$CHANGED" ]; then
    REASON="this PR's diff against its base is empty"
    return
  fi
  local re
  for re in "$SELF_RE" "$HARNESS_PATHS_RE"; do
    if ! grep_hit 0 "$re"; then
      REASON="grep over this PR's diff failed, so the harness guard is unread"
      return
    fi
    if [ -n "$HIT" ]; then
      GUARD="true"
      REASON="this PR changes '${HIT}', part of this job's own harness, so it can have caused the setup death"
      return
    fi
  done
  if [ "$first_class" = "frontend" ]; then
    if ! grep_hit 1 "$INERT_RE"; then
      REASON="grep over this PR's diff failed, so the frontend guard is unread"
      return
    fi
    if [ -n "$HIT" ]; then
      GUARD="true"
      REASON="'${first}' reads this PR's frontend tree and the PR changes '${HIT}', so the failure can be this PR's own"
      return
    fi
  fi
  GUARD="false"
}
read_guard

if [ "$GUARD" = "false" ]; then
  echo "::error title=$(esc_prop "${JOB} SETUP FAILED (not a verdict on this PR)")::$(esc_data "step '${first}' failed before the verdict step ran and this PR cannot have caused it: re-run once")"
else
  withhold "setup step '${first}' failed, but the guard read ${GUARD}: ${REASON}. This red is left to read as a verdict on this change."
fi
exit 0
