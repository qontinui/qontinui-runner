#!/bin/bash
# classify-branch-state.sh — is this checkout's branch WORK, or a SHADOW of work
# that already landed?
#
# WHY THIS EXISTS
#
# On a fleet whose merge authority REBASE-lands, a branch that shipped keeps its
# commits locally under different SHAs. Those commits are byte-identical
# duplicates of what is already on origin/main, and every cheap test says the
# opposite:
#
#   git merge-base --is-ancestor HEAD origin/main   -> "not landed"   (landed)
#   git rev-list --count origin/main..HEAD          -> "2 ahead"      (0 unique)
#   gh pr view                                      -> CLOSED, mergedAt null
#   git status --porcelain                          -> "not the default branch"
#
# So `scan-worktree-wip.sh` reports UNPUSHED peer work, `/pull-scoped` refuses,
# every peer session coordinates around a ghost, and the checkout stays parked
# on that branch falling further behind every day. None of those tools is wrong;
# they are all reading the only evidence they have. What was missing is a test
# that separates SUPERSEDED from UNSHIPPED.
#
# Measured on this box 2026-09-04, and it is why this file exists rather than
# being a nice idea: the reconnaissance for the very plan that commissioned this
# script flagged `qontinui-claude-config-wt-custodyinv` (branch
# `feat/custody-emit-site-invariant`, commit 8b67d0a) as "UNPUSHED peer work --
# COORDINATE, do not proceed", while that branch's PR #708 was MERGED. This
# script calls that case LANDED_DUPLICATE in 23 ms.
#
# WHAT IT ANSWERS, AND WHAT IT REFUSES TO ANSWER
#
#   It answers: "is the content on this branch already on the upstream default
#   branch?" That is decidable from the object store.
#
#   It does NOT answer: "is anyone working in this checkout?" That is not
#   decidable from disk, and served policy `coordination`
#   `overlap-read-is-not-proof-of-a-free-file` records why in its BOUNDS -- a
#   reader's own `git status` rewrites `.git/index`, so an abandoned tree reads
#   as seconds old. This script emits NO liveness judgement and takes NO action.
#   A caller that wants to switch a branch must resolve liveness separately.
#
# THE GOVERNING ASYMMETRY IS THE OPPOSITE OF THE SIBLING SCANNER'S.
#
# `scan-worktree-wip.sh` exists to prevent a false ALL-CLEAR, so it fails toward
# "someone holds this". This script exists to authorise a *destructive* move
# (returning a checkout to its default branch), so it fails toward "leave it
# alone". A wrong LANDED_DUPLICATE is the only way this tooling can destroy
# work. Hence, in order:
#
#   * UNCOMMITTED CONTENT FORCES UNIQUE_WIP, unconditionally, before any commit
#     is examined. Staged, unstaged, unmerged AND untracked. This is the primary
#     safety property and it is checked first.
#   * INCOMPLETE IS ITS OWN EXIT CODE. It is never collapsed into UNIQUE_WIP
#     (that reproduces the deadlock this exists to break) and never into
#     LANDED_DUPLICATE (that authorises a destructive action on an unknown).
#   * A MERGE COMMIT in the ahead range that would otherwise produce
#     LANDED_DUPLICATE downgrades to INCOMPLETE. A merge's content is not a
#     patch, so patch-id equivalence is undefined for it, and an "evil merge"
#     (content introduced during conflict resolution) is exactly the shape that
#     would be silently thrown away.
#
# HOW IT DECIDES -- CONTENT FIRST, PATCH-ID AS THE TIEBREAKER
#
#   1. Uncommitted content?                       -> UNIQUE_WIP. Stop.
#   2. Zero commits ahead of the upstream ref?    -> LANDED_DUPLICATE. Stop.
#      (HEAD is an ancestor: there is nothing unique to lose. The common case
#      is a checkout already ON its default branch, merely behind.)
#   3. CONTENT COMPARISON. Take the files the ahead-commits changed net of the
#      merge base, and the files whose content differs between HEAD and the
#      upstream tip. If those two sets do not INTERSECT, every file this branch
#      touched already reads identically on the upstream -> LANDED_DUPLICATE.
#      Two `git diff --name-only` calls, ~1 s on the largest repo here.
#   4. Otherwise the content test is INCONCLUSIVE, and only then does patch-id
#      run -- `git cherry <upstream> HEAD`, which is exactly a patch-id sweep
#      SCOPED to `merge-base..upstream`. Measured on this box: 22 ms on
#      qontinui-runner (38 behind), 296 ms on qontinui-web (144 behind), 327 ms
#      on qontinui-dev-notes (1338 behind).
#
# WHY THE CONTENT TEST CANNOT BE THE WHOLE ANSWER -- read this before
# "simplifying" step 4 away. The commissioning plan specified patch-id as the
# tiebreaker "for MIXED only", and that under-specifies the primitive: the
# content test is ONE-SIDED. Its EMPTY answer is conclusive (nothing this branch
# touched differs, so nothing is unique); its NON-EMPTY answer is not, because
# the upstream MOVES ON after a branch lands. Measured on the plan's own
# flagship case: `qontinui-claude-config-wt-custodyinv` has a landed commit and
# a 2482-line content diff against origin/main on the same files, purely because
# main kept changing them for 47 commits afterwards. Restricting patch-id to the
# MIXED arm would misclassify the one case the plan cites as evidence. So the
# content test is the FAST PATH, not the only path, and it is still worth having
# because it settles the freshly-landed case with no patch-id work at all.
#
# WHY NOT PATCH-ID FIRST. The survey that motivated the plan ran a hand-rolled
# patch-id sweep across every commit on main since a date floor and took over
# five minutes. `git cherry` does not do that: its upstream side is bounded by
# the merge base, which is the whole difference. Do not reintroduce a date floor.
#
# EVERY VERDICT IS A FLOOR
#
# This script NEVER fetches -- the same decision `.claude/hooks/
# landed-not-live-staleness.sh` records as D4, for the same reason: a fetch at
# read time is a network call on a hot path and it mutates the repo. So every
# count and every verdict is relative to the remote-tracking ref AS LAST
# FETCHED, and the amount that has really landed can only be LARGER than what is
# reported. The output prints TWO ages for exactly this reason, and they answer
# different questions, so they are never merged into one field:
#
#   upstream_tip_age     how old the upstream TIP COMMIT is (`git log -1 %cr`).
#                        A quiet repo's tip can be weeks old on a ref fetched a
#                        minute ago; this is NOT the ref's freshness.
#   upstream_fetched_at  WHEN the ref was last fetched: the mtime of FETCH_HEAD
#                        (newest of the per-worktree and the common git dir),
#                        falling back to the newest reflog entry of the
#                        remote-tracking ref (which only moves when a fetch
#                        CHANGED it, so it is a lower bound). `null` when neither
#                        exists -- UNKNOWN, never "just now".
#                        `upstream_fetched_at_source` names which one answered.
#
# Until 2026-09-13 the tip age was printed as "as last fetched, <tip age>" and
# emitted under a key named for a generic RELATIVE AGE, which read a commit's
# age as the ref's (plan 2026-09-13-nightly-return-to-main-sweep, Phase 1c).
#
# The direction of that error is the safe one and it is worth stating plainly: a
# stale upstream ref inflates UNIQUE_WIP and can never fabricate a
# LANDED_DUPLICATE. A commit cannot be found equivalent to an upstream commit
# that has not been fetched.
#
# READ-ONLY. `--no-optional-locks` on EVERY git invocation, matching
# `scan-worktree-wip.sh:_git_c` so the two compose: classification never fights
# a peer's index.lock and never rewrites the stat cache in a shared checkout.
# No command here writes a ref, an index, a config or a file in the repo.
#
# USAGE
#   classify-branch-state.sh [options] [<checkout>]
#
#   classify-branch-state.sh                       # classify the cwd's checkout
#   classify-branch-state.sh /path/to/qontinui-web
#   classify-branch-state.sh --json /path/to/repo  # one JSON object
#   classify-branch-state.sh --quiet /path/to/repo # the verdict token, alone
#
# OPTIONS
#   --upstream <ref>  Compare against <ref> instead of the resolved default
#                     branch. Also $QONTINUI_CLASSIFY_UPSTREAM. A ref that does
#                     not exist locally is INCOMPLETE, never a verdict.
#   --json            One JSON object on stdout, nothing else.
#   --quiet           Exactly the verdict token on one line, nothing else.
#                     For callers that want the word as well as the exit code
#                     (`scan-worktree-wip.sh --classify` uses this).
#   -h, --help        This text.
#
# UPSTREAM RESOLUTION, in order. The first that resolves to a commit wins:
#   1. --upstream / $QONTINUI_CLASSIFY_UPSTREAM
#   2. refs/remotes/origin/HEAD          (the remote's declared default branch)
#   3. origin/main
#   4. origin/master
#   NOT `@{upstream}`. A feature branch's own upstream is `origin/<branch>`,
#   which answers "have I pushed?" -- a different question, and one that reads
#   LANDED_DUPLICATE for a branch that was pushed and never merged.
#
# EXIT CODES -- the mapping is the interface; callers branch on it.
#   0  LANDED_DUPLICATE  every ahead-commit's content is already on the upstream
#                        default branch. Nothing unique here. Safe to return the
#                        checkout to its default branch.
#   1  UNIQUE_WIP        content exists that the upstream does not have -- OR
#                        the working tree is dirty, which forces this verdict
#                        whatever the commits say.
#   2  MIXED             some ahead-commits landed, some did not.
#   3  INCOMPLETE        the question could not be answered. NOT a verdict, and
#                        never to be read as either of 0 or 1. Causes: no
#                        upstream ref fetched, unrelated histories, an unborn
#                        HEAD, a failed git call, or an unclassifiable merge
#                        commit in an otherwise-landed range.
#   4  usage error       bad option, missing argument, no such directory.
#
#   Note that 2 here is MIXED, where the sibling `scan-worktree-wip.sh` uses 2
#   for usage errors. The plan fixed 0/1/2/3 for the four answers, so usage
#   moved to 4. A caller composing the two scripts must not share a case arm.
# ---- END HELP

set -u

# -- Isolate the scoped git queries ------------------------------------------
# Identical reasoning to scan-worktree-wip.sh: an INHERITED GIT_DIR skips
# repository discovery, so `git -C <path>` silently answers about a DIFFERENT
# repository -- well-formed, nothing errors, nothing is UNKNOWN. git exports
# GIT_DIR into every hook's environment and this workspace installs per-clone
# hooks, so the poison is ordinary. Here the consequence is worse than a wrong
# report: a caller acts on the verdict.
#
# GATE ON THE FUNCTION, NOT ON THE FILE -- a lib that sources cleanly but
# defines nothing leaves the strip unapplied.
_cbs_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

_cbs_lib="$_cbs_dir/lib/git-scope.sh"
if [ -r "$_cbs_lib" ]; then
  # shellcheck source=lib/git-scope.sh
  . "$_cbs_lib"
fi
if declare -F git_scope_strip >/dev/null 2>&1; then
  git_scope_strip
elif [ -n "${GIT_DIR+s}${GIT_WORK_TREE+s}${GIT_COMMON_DIR+s}" ]; then
  # Absence of the lib is not automatically danger -- with no scope-redirecting
  # variable set there is nothing to strip, and a standalone copy of this script
  # keeps working. With one SET there is, and every answer below would be about
  # another repository.
  echo "classify-branch-state: FATAL - lib/git-scope.sh is not usable ($_cbs_lib) AND this process carries GIT_DIR / GIT_WORK_TREE / GIT_COMMON_DIR, so every query below would answer about ANOTHER repository. Refusing." >&2
  exit 4
fi

# -- Native path for `git -C` ------------------------------------------------
# Same MSYS boundary as the sibling: candidates are POSIX-spelled and a native
# git.exe cannot open `/d/<root>/...` when a caller has exported
# MSYS_NO_PATHCONV=1 (several runbooks in this fleet do, and it is inherited).
# `native_path_w` is the backslash spelling, correct because the value is handed
# straight to git as one `-C` argument and never composed with anything.
_cbs_lib="$_cbs_dir/lib/native-path.sh"
if [ -r "$_cbs_lib" ]; then
  # shellcheck source=lib/native-path.sh
  . "$_cbs_lib"
fi
if ! declare -F native_path_w >/dev/null 2>&1; then
  if command -v cygpath >/dev/null 2>&1; then
    echo "classify-branch-state: FATAL - lib/native-path.sh is not usable ($_cbs_lib) AND cygpath is present, so this is an MSYS box: the checkout would reach git.exe in a spelling it cannot open. Refusing." >&2
    exit 4
  fi
  native_path_w() { printf '%s\n' "$1"; }
fi
unset _cbs_lib

# ---------------------------------------------------------------------------
# args

CHECKOUT=""
UPSTREAM_OVERRIDE="${QONTINUI_CLASSIFY_UPSTREAM:-}"
MODE="human"

while [ $# -gt 0 ]; do
  case "$1" in
    --upstream)
      shift
      [ $# -gt 0 ] || { echo "classify-branch-state: --upstream needs a ref" >&2; exit 4; }
      UPSTREAM_OVERRIDE="$1"; shift ;;
    --json)  MODE="json";  shift ;;
    --quiet) MODE="quiet"; shift ;;
    # Print the header up to the sentinel rather than a hard-coded line range,
    # which silently truncated on every header edit in the sibling.
    -h|--help)
      sed -n '2,/^# ---- END HELP/p' "$0" | sed '$d' | sed 's/^#\{0,1\} \{0,1\}//'
      exit 0 ;;
    --) shift
        [ $# -gt 0 ] && { CHECKOUT="$1"; shift; }
        [ $# -eq 0 ] || { echo "classify-branch-state: one checkout at a time" >&2; exit 4; } ;;
    -*) echo "classify-branch-state: unknown option $1" >&2; exit 4 ;;
    *)  [ -z "$CHECKOUT" ] || { echo "classify-branch-state: one checkout at a time" >&2; exit 4; }
        CHECKOUT="$1"; shift ;;
  esac
done

[ -n "$CHECKOUT" ] || CHECKOUT="$PWD"
[ -d "$CHECKOUT" ] || { echo "classify-branch-state: not a directory: $CHECKOUT" >&2; exit 4; }
CHECKOUT="$(cd "$CHECKOUT" 2>/dev/null && pwd)" || { echo "classify-branch-state: cannot enter $CHECKOUT" >&2; exit 4; }

NATIVE_CO="$(native_path_w "$CHECKOUT")"
# An EMPTY conversion must never reach git: `git -C ""` is a documented NO-OP
# that leaves git in the CURRENT directory and exits 0, so the classification
# would be of the CALLER's tree reported under the target's name. The sibling
# records the same trap; here it could authorise a branch switch in the wrong
# repository.
[ -n "$NATIVE_CO" ] || { echo "classify-branch-state: path conversion produced an empty path for $CHECKOUT" >&2; exit 4; }

# ---------------------------------------------------------------------------
# scratch + git

TMPD="$(mktemp -d)" || { echo "classify-branch-state: mktemp failed" >&2; exit 4; }
trap 'rm -rf "$TMPD"' EXIT

# EVERY git call goes through here. `--no-optional-locks` is the whole point:
# without it a `git status` in a shared checkout takes index.lock and refreshes
# the stat cache, which is both a write and a fight with the peer whose work we
# are trying to classify. `core.quotePath=false` so a non-ASCII path comes back
# as itself and can be compared against the other list rather than as an octal
# escape that matches nothing.
_git() { git -C "$NATIVE_CO" --no-optional-locks -c core.quotePath=false "$@"; }

# ---------------------------------------------------------------------------
# state, and the single exit funnel

VERDICT=""
SETTLED_BY=""
INCOMPLETE_REASON=""
DETAIL=()

REPO=""
BRANCH=""
DETACHED="false"
AHEAD=""
BEHIND=""
AHEAD_MERGES=""
DIRTY_TRACKED=0
DIRTY_UNTRACKED=0
UPSTREAM=""
UPSTREAM_SOURCE=""
UPSTREAM_TIP=""
UPSTREAM_AGE=""
UPSTREAM_DATE=""
UPSTREAM_FETCHED_AT=""
UPSTREAM_FETCHED_SOURCE=""
LANDED=()
UNLANDED=()

# JSON string escaping: backslash, double quote, tab; control chars stripped.
# Same helper the fleet already ships in capability-doctor.sh, copied rather
# than sourced so this script runs standalone from any checkout.
json_escape() {
  printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/	/\\t/g' | tr -d '\n\r'
}

json_str()  { if [ -n "${1:-}" ]; then printf '"%s"' "$(json_escape "$1")"; else printf 'null'; fi; }
json_num()  { case "${1:-}" in ''|*[!0-9]*) printf 'null' ;; *) printf '%s' "$1" ;; esac; }
json_arr()  {
  local first=1 e
  printf '['
  for e in "$@"; do
    [ "$first" = 1 ] || printf ','
    first=0
    printf '"%s"' "$(json_escape "$e")"
  done
  printf ']'
}

# The ref's FETCH time, in words, for the human surfaces. Deliberately never
# the tip commit's age: that is the conflation Phase 1c of plan
# 2026-09-13-nightly-return-to-main-sweep removed.
_fetched_phrase() {
  if [ -n "$UPSTREAM_FETCHED_AT" ]; then
    printf '%s, per %s' "$UPSTREAM_FETCHED_AT" "$UPSTREAM_FETCHED_SOURCE"
  else
    printf 'at an UNKNOWN time -- no FETCH_HEAD and no reflog entry for the ref'
  fi
}

exit_code_for() {
  case "$1" in
    LANDED_DUPLICATE) printf '0' ;;
    UNIQUE_WIP)       printf '1' ;;
    MIXED)            printf '2' ;;
    *)                printf '3' ;;
  esac
}

emit_and_exit() {
  local rc; rc="$(exit_code_for "$VERDICT")"

  if [ "$MODE" = "quiet" ]; then
    printf '%s\n' "$VERDICT"
    exit "$rc"
  fi

  if [ "$MODE" = "json" ]; then
    printf '{'
    printf '"verdict":%s,'            "$(json_str "$VERDICT")"
    printf '"exit_code":%s,'          "$rc"
    printf '"settled_by":%s,'         "$(json_str "$SETTLED_BY")"
    printf '"incomplete_reason":%s,'  "$(json_str "$INCOMPLETE_REASON")"
    printf '"checkout":%s,'           "$(json_str "$CHECKOUT")"
    printf '"repo":%s,'               "$(json_str "$REPO")"
    printf '"branch":%s,'             "$(json_str "$BRANCH")"
    printf '"detached":%s,'           "$DETACHED"
    printf '"upstream_ref":%s,'       "$(json_str "$UPSTREAM")"
    printf '"upstream_source":%s,'    "$(json_str "$UPSTREAM_SOURCE")"
    printf '"upstream_tip":%s,'       "$(json_str "$UPSTREAM_TIP")"
    printf '"upstream_tip_age":%s,'   "$(json_str "$UPSTREAM_AGE")"
    printf '"upstream_committed_at":%s,' "$(json_str "$UPSTREAM_DATE")"
    printf '"upstream_fetched_at":%s,' "$(json_str "$UPSTREAM_FETCHED_AT")"
    printf '"upstream_fetched_at_source":%s,' "$(json_str "$UPSTREAM_FETCHED_SOURCE")"
    printf '"ahead":%s,'              "$(json_num "$AHEAD")"
    printf '"behind":%s,'             "$(json_num "$BEHIND")"
    printf '"ahead_merge_commits":%s,' "$(json_num "$AHEAD_MERGES")"
    printf '"dirty_tracked":%s,'      "$(json_num "$DIRTY_TRACKED")"
    printf '"dirty_untracked":%s,'    "$(json_num "$DIRTY_UNTRACKED")"
    printf '"landed_commits":%s,'     "$(json_arr ${LANDED[@]+"${LANDED[@]}"})"
    printf '"unlanded_commits":%s,'   "$(json_arr ${UNLANDED[@]+"${UNLANDED[@]}"})"
    # Two constants, and they are not decoration. `floor` says the verdict is
    # relative to an already-fetched ref; `liveness` is explicitly null because
    # this script refuses to answer that question at all, and a consumer that
    # finds the key absent might otherwise infer the field simply was not
    # computed this run.
    printf '"floor":true,'
    printf '"fetched":false,'
    printf '"liveness":null'
    printf '}\n'
    exit "$rc"
  fi

  printf 'classify-branch-state: %s\n' "$CHECKOUT"
  printf '  repo=%s branch=%s%s\n' "${REPO:-<unknown>}" "${BRANCH:-<unknown>}" \
    "$( [ "$DETACHED" = "true" ] && printf ' (detached HEAD)' )"
  if [ -n "$UPSTREAM" ]; then
    printf '  upstream=%s (%s), tip committed %s, ref last fetched %s\n' "$UPSTREAM" "$UPSTREAM_SOURCE" \
      "${UPSTREAM_AGE:-<age unknown>}" "$(_fetched_phrase)"
  else
    printf '  upstream=<unresolved>\n'
  fi
  # Always printed, `?` where a count was never reached. The dirty census rides
  # on this line and it is the one that settles the safety property, so a path
  # that short-circuits before ahead/behind were computed must still show it.
  printf '  ahead=%s behind=%s  uncommitted: %s tracked, %s untracked\n' \
    "${AHEAD:-?}" "${BEHIND:-?}" "$DIRTY_TRACKED" "$DIRTY_UNTRACKED"
  printf '\n  VERDICT: %s\n' "$VERDICT"
  local line
  for line in ${DETAIL[@]+"${DETAIL[@]}"}; do printf '    %s\n' "$line"; done
  [ -n "$SETTLED_BY" ] && printf '    (settled by: %s)\n' "$SETTLED_BY"
  printf '\n'
  # The floor disclaimer is not boilerplate and is printed on EVERY verdict,
  # including INCOMPLETE. A reader who acts on a count without knowing when the
  # ref was last fetched is the failure mode the commissioning plan's §6 names.
  printf '  FLOOR, not a measurement of the remote. Nothing here fetches, so every\n'
  printf '  count and the verdict itself are relative to %s AS LAST FETCHED\n' "${UPSTREAM:-the upstream ref}"
  printf '  (%s).\n' "$(_fetched_phrase)"
  printf '  The amount that has really landed can only be LARGER.\n'
  printf '  A stale ref inflates UNIQUE_WIP; it can never fabricate a LANDED_DUPLICATE.\n'
  printf '\n'
  printf '  This says NOTHING about whether anyone is working here. It answers "is this\n'
  printf '  content already upstream", which is decidable; it does not answer "is this\n'
  printf '  checkout live", which is not (served policy `coordination`\n'
  printf '  `overlap-read-is-not-proof-of-a-free-file`). No action was taken.\n'
  exit "$rc"
}

incomplete() {
  VERDICT="INCOMPLETE"
  INCOMPLETE_REASON="$1"
  DETAIL=(
    "$1"
    "INCOMPLETE is not a verdict. It is neither LANDED_DUPLICATE nor UNIQUE_WIP:"
    "reading it as the first would authorise a destructive move on an unknown,"
    "and reading it as the second reproduces the deadlock this tool exists to break."
  )
  emit_and_exit
}

# ---------------------------------------------------------------------------
# repo name, WITHOUT a spawn
#
# Same derivation as scan-worktree-wip.sh's `_repo_of` and for the same reason:
# the DIRECTORY basename is wrong for a second full clone
# (`qontinui-runner-clone-unwedge`) and for a linked worktree
# (`agent-worktrees/<uuid>/<repo>`, `qr-wt-killtree`). Copied rather than
# factored into lib/: this script must run standalone from a checkout that has
# no lib/ beside it, and factoring it would mean editing the sibling's hot path.
# The name is cosmetic here -- it labels the report and gives
# `scan-worktree-wip.sh --classify` a column key -- so a failure to derive it is
# NOT INCOMPLETE.
_origin_repo_of() {
  local cfg="$1/.git/config" line in_origin=0 url
  [ -f "$cfg" ] || return 1
  while IFS= read -r line; do
    line="${line%$'\r'}"
    case "$line" in
      '['*)
        case "$line" in
          '[remote "origin"]'*) in_origin=1 ;;
          *) in_origin=0 ;;
        esac
        continue ;;
    esac
    [ "$in_origin" = 1 ] || continue
    case "$line" in
      *url*=*)
        url="${line#*=}"; url="${url# }"; url="${url% }"
        url="${url%.git}"; url="${url%/}"
        url="${url//$'\\'//}"
        [ -n "$url" ] || return 1
        printf '%s' "${url##*/}"
        return 0 ;;
    esac
  done < "$cfg"
  return 1
}

_repo_of() {
  local d="$1" line main b
  if [ -d "$d/.git" ]; then
    _origin_repo_of "$d" && return 0
    b="${d%/}"; printf '%s' "${b##*/}"; return 0
  fi
  [ -f "$d/.git" ] || return 1
  IFS= read -r line < "$d/.git" || return 1
  case "$line" in gitdir:*) ;; *) return 1 ;; esac
  line="${line#gitdir:}"; line="${line# }"
  line="${line//\\//}"
  line="${line%$'\r'}"
  case "$line" in
    */.git/worktrees/*) main="${line%%/.git/worktrees/*}" ;;
    */.git/modules/*)   main="${line%%/.git/modules/*}" ;;
    *) return 1 ;;
  esac
  main="${main%/}"
  [ -n "$main" ] || return 1
  printf '%s' "${main##*/}"
}

REPO="$(_repo_of "$CHECKOUT" 2>/dev/null)" || REPO=""

# ---------------------------------------------------------------------------
# SPAWN 1 - status: branch, detached-ness, and the dirty census
#
# ONE call does three jobs (`--porcelain=v2 --branch`), for the same spawn-cost
# reason the sibling records: a git process under Git Bash on Windows costs
# seconds under load. `-uall` because the default `-unormal` collapses a wholly
# new directory to one `newdir/` entry, and a peer's brand-new feature directory
# is exactly the shape that must force UNIQUE_WIP.
#
# Into a FILE, not `$( )`: command substitution DISCARDS NUL bytes, which
# collapses `-z` output into one unparseable blob. Here that would read as a
# clean tree -- i.e. it would delete the primary safety property.
if ! _git status --porcelain=v2 --branch -z -uall > "$TMPD/status" 2>"$TMPD/status.err"; then
  incomplete "git status failed in this checkout: $(head -1 "$TMPD/status.err" 2>/dev/null | tr -d '\r'). Common causes: not a git repository, safe.directory ownership, a repo mid-rebase."
fi

expect_orig=0
while IFS= read -r -d '' rec; do
  if [ "$expect_orig" = 1 ]; then
    # A rename's ORIGINAL path arrives as a separate NUL record and MUST be
    # consumed, or it is read as another entry. It is not counted again -- the
    # rename was already counted on its `2 ` line.
    expect_orig=0
    continue
  fi
  case "$rec" in
    '# branch.head '*) BRANCH="${rec#\# branch.head }" ;;
    '# branch.oid '*)
      case "${rec#\# branch.oid }" in
        '(initial)') UNBORN=1 ;;
      esac ;;
    '# '*) ;;
    '1 '*) DIRTY_TRACKED=$((DIRTY_TRACKED + 1)) ;;
    'u '*) DIRTY_TRACKED=$((DIRTY_TRACKED + 1)) ;;
    '2 '*) DIRTY_TRACKED=$((DIRTY_TRACKED + 1)); expect_orig=1 ;;
    '? '*) DIRTY_UNTRACKED=$((DIRTY_UNTRACKED + 1)) ;;
  esac
done < "$TMPD/status"

if [ "$BRANCH" = "(detached)" ]; then DETACHED="true"; BRANCH="<detached>"; fi

# ---------------------------------------------------------------------------
# Upstream resolution. Reported even when the verdict is already settled by the
# dirty tree, because the contract requires the ref's AGE in every output and a
# reader cannot size a floor without it.

_resolve_upstream() {
  local cand
  if [ -n "$UPSTREAM_OVERRIDE" ]; then
    # An explicitly named ref that does not resolve is INCOMPLETE, never a
    # silent fall-through to origin/main: the caller asked a specific question
    # and would otherwise get an answer to a different one.
    if UPSTREAM_TIP="$(_git rev-parse --verify --quiet "${UPSTREAM_OVERRIDE}^{commit}" 2>/dev/null)" && [ -n "$UPSTREAM_TIP" ]; then
      UPSTREAM="$UPSTREAM_OVERRIDE"; UPSTREAM_SOURCE="explicit --upstream"
      return 0
    fi
    return 1
  fi

  # The remote's own declared default branch. Often absent or dangling (a clone
  # made with --single-branch, or a default branch that was later renamed), so
  # its failure is ordinary and falls through rather than reporting anything.
  if cand="$(_git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null)" && [ -n "$cand" ]; then
    if UPSTREAM_TIP="$(_git rev-parse --verify --quiet "${cand}^{commit}" 2>/dev/null)" && [ -n "$UPSTREAM_TIP" ]; then
      UPSTREAM="$cand"; UPSTREAM_SOURCE="refs/remotes/origin/HEAD"
      return 0
    fi
  fi

  for cand in origin/main origin/master; do
    if UPSTREAM_TIP="$(_git rev-parse --verify --quiet "${cand}^{commit}" 2>/dev/null)" && [ -n "$UPSTREAM_TIP" ]; then
      UPSTREAM="$cand"; UPSTREAM_SOURCE="fallback probe"
      return 0
    fi
  done
  UPSTREAM_TIP=""
  return 1
}

if _resolve_upstream; then
  # %cr and %cI together, one spawn. The relative form is what a human sizes a
  # floor with; the ISO form is what a machine consumer compares.
  if _ages="$(_git log -1 --format='%cr%x1f%cI' "$UPSTREAM" 2>/dev/null)"; then
    UPSTREAM_AGE="${_ages%%$'\x1f'*}"
    UPSTREAM_DATE="${_ages#*$'\x1f'}"
  fi
fi

# ---------------------------------------------------------------------------
# WHEN the upstream ref was last fetched -- a different fact from the tip's age.
#
# First choice: FETCH_HEAD's mtime. git rewrites FETCH_HEAD on every fetch that
# gets as far as writing refs, whether or not anything changed, so it is the
# closest thing to "last fetched" a repository records. It is PER-WORKTREE
# (a linked worktree's lives in its private git dir) while a fetch run from the
# primary writes the common dir's, so both are read and the newer wins. Caveat,
# stated rather than hidden: it is the time of the last fetch of ANY refspec
# from ANY remote, not specifically of this ref.
#
# Fallback: the newest reflog entry of refs/remotes/<upstream>. A reflog entry
# is written only when a fetch MOVED the ref, so this is a lower bound on
# freshness (a fetch that found nothing new leaves no entry). Reported with its
# source so a reader can tell the two apart.
#
# Neither -> empty, which emits `null`: UNKNOWN, never "just now". Stat and
# date are probed GNU-first then BSD, so this runs on Linux, macOS and Git Bash.
_mtime_epoch() { stat -c %Y "$1" 2>/dev/null || stat -f %m "$1" 2>/dev/null; }
_epoch_iso() {
  case "${1:-}" in ''|*[!0-9]*) return 1 ;; esac
  date -u -d "@$1" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -r "$1" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null
}
if [ -n "$UPSTREAM" ]; then
  _fh_best=""
  for _gd in "$(_git rev-parse --absolute-git-dir 2>/dev/null)" \
             "$(_git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)"; do
    _gd="${_gd%$'\r'}"
    [ -n "$_gd" ] && [ -f "$_gd/FETCH_HEAD" ] || continue
    _m="$(_mtime_epoch "$_gd/FETCH_HEAD")"
    case "$_m" in ''|*[!0-9]*) continue ;; esac
    if [ -z "$_fh_best" ] || [ "$_m" -gt "$_fh_best" ]; then _fh_best="$_m"; fi
  done
  if [ -n "$_fh_best" ] && _iso="$(_epoch_iso "$_fh_best")" && [ -n "$_iso" ]; then
    UPSTREAM_FETCHED_AT="$_iso"
    UPSTREAM_FETCHED_SOURCE="FETCH_HEAD mtime"
  else
    # Only a remote-tracking ref has a fetch reflog worth reading; an explicit
    # --upstream naming a local branch would report its COMMIT history instead.
    case "$UPSTREAM" in
      refs/remotes/*) _rref="$UPSTREAM" ;;
      origin/*)       _rref="refs/remotes/$UPSTREAM" ;;
      *)              _rref="" ;;
    esac
    if [ -n "$_rref" ] \
       && _gd_line="$(_git log -g -1 --date=unix --format='%gd' "$_rref" 2>/dev/null)" \
       && [ -n "$_gd_line" ]; then
      _gd_line="${_gd_line%$'\r'}"
      _ep="${_gd_line##*@\{}"; _ep="${_ep%\}}"
      if _iso="$(_epoch_iso "$_ep")" && [ -n "$_iso" ]; then
        UPSTREAM_FETCHED_AT="$_iso"
        UPSTREAM_FETCHED_SOURCE="reflog of $_rref (newest entry; moves only when a fetch changed the ref)"
      fi
    fi
  fi
  unset _fh_best _gd _m _iso _rref _gd_line _ep
fi

# ---------------------------------------------------------------------------
# 1. UNCOMMITTED CONTENT FORCES UNIQUE_WIP. Unconditionally, and first.
#
# The commissioning plan's §6 names a wrong LANDED_DUPLICATE as the ONLY way
# this tooling can destroy work, and an uncommitted edit is the content most
# certainly not upstream -- it is in no commit at all, so no patch-id and no
# tree comparison can ever see it. Untracked files count: a peer's brand-new
# file is content, and the fleet's own custody recorder documents that a WIP
# snapshot cannot reach untracked files either.
#
# The breakdown (tracked vs untracked) is reported so a downstream sweeper can
# apply its own policy to, say, an untracked-only tree -- but the VERDICT here
# is UNIQUE_WIP either way. This function does not have the context to decide
# that a stray file is junk.
if [ "$((DIRTY_TRACKED + DIRTY_UNTRACKED))" -gt 0 ]; then
  VERDICT="UNIQUE_WIP"
  SETTLED_BY="uncommitted content in the working tree (checked before any commit was examined)"
  DETAIL=(
    "The working tree holds $DIRTY_TRACKED tracked change(s) and $DIRTY_UNTRACKED untracked file(s)."
    "That content is in no commit, so no upstream comparison can account for it."
    "This verdict is unconditional and overrides whatever the ahead-commits say."
  )
  emit_and_exit
fi

# From here on the tree is clean, so every remaining question is about commits.

if [ "${UNBORN:-0}" = 1 ]; then
  incomplete "HEAD is unborn (no commits on this branch), so there is nothing to compare against $([ -n "$UPSTREAM" ] && printf '%s' "$UPSTREAM" || printf 'any upstream'). A clean unborn checkout is not evidence that its branch landed."
fi

if [ -z "$UPSTREAM" ]; then
  if [ -n "$UPSTREAM_OVERRIDE" ]; then
    incomplete "the ref named by --upstream ('$UPSTREAM_OVERRIDE') does not resolve to a commit in this checkout. Nothing was compared."
  fi
  incomplete "no upstream default branch is fetched in this checkout (tried refs/remotes/origin/HEAD, origin/main, origin/master). Nothing was compared, and this script never fetches."
fi

# ---------------------------------------------------------------------------
# SPAWN: ahead / behind against the DEFAULT branch, in one call.
#
# `--left-right --count A...B` prints "<left> <right>": commits reachable from
# the upstream but not HEAD (behind), then from HEAD but not the upstream
# (ahead). Deliberately against the default branch and NOT `@{upstream}` -- see
# the UPSTREAM RESOLUTION note in the header.
if ! _lr="$(_git rev-list --left-right --count "${UPSTREAM}...HEAD" 2>/dev/null)" || [ -z "$_lr" ]; then
  incomplete "cannot count commits between HEAD and $UPSTREAM -- most likely UNRELATED HISTORIES (no merge base). A branch with no common ancestor cannot be classified as landed or unique by this method."
fi
BEHIND="${_lr%%[[:space:]]*}"
AHEAD="${_lr##*[[:space:]]}"
case "$BEHIND$AHEAD" in *[!0-9]*) incomplete "unparseable rev-list output ('$_lr') counting HEAD against $UPSTREAM." ;; esac

# ---------------------------------------------------------------------------
# 2. Nothing ahead -> nothing unique. Stop before any content work.
#
# This is the ordinary case for a checkout sitting ON its default branch merely
# behind (measured here: ui-bridge, 45 behind, 0 ahead, clean). HEAD is an
# ancestor of the upstream tip, so a fast-forward loses nothing by definition.
if [ "$AHEAD" = 0 ]; then
  VERDICT="LANDED_DUPLICATE"
  SETTLED_BY="no commits ahead of $UPSTREAM -- HEAD is an ancestor of it"
  DETAIL=(
    "HEAD carries no commit that $UPSTREAM does not already contain."
    "There is nothing unique in this checkout to lose; it is $BEHIND commit(s) behind."
  )
  emit_and_exit
fi

# ---------------------------------------------------------------------------
# 3. CONTENT COMPARISON -- the fast path, and the ONLY conclusive form of it is
#    the empty one.
#
#    touched   = files the ahead-commits changed net of the merge base
#                (three-dot: `UPSTREAM...HEAD` is `merge-base..HEAD`)
#    differing = files whose content differs between the upstream tip and HEAD
#                (two-dot)
#
#    Empty INTERSECTION means every file this branch touched already reads
#    identically upstream -> LANDED_DUPLICATE.
#
#    WHY AN INTERSECTION RATHER THAN `git diff HEAD <upstream> -- <touched>`.
#    Passing the touched files as PATHSPECS has two failure modes and one of
#    them is unsafe: a pathspec has glob magic, so a filename containing `[abc]`
#    is a CHARACTER CLASS and would silently match FEWER files than intended --
#    an under-matched scope reads as "no difference" and produces a wrong
#    LANDED_DUPLICATE. (`:(literal)` fixes that but is a colon-prefixed argument,
#    which MSYS path conversion mangles.) The second is an argument-list ceiling
#    on a branch touching thousands of files. Two whole-tree name lists and a
#    `comm` have neither, and cost one spawn more.
if ! _git diff --name-only -z "${UPSTREAM}...HEAD" > "$TMPD/touched" 2>/dev/null; then
  incomplete "could not list the files the ahead-commits touched (git diff $UPSTREAM...HEAD failed)."
fi
if ! _git diff --name-only -z "$UPSTREAM" HEAD > "$TMPD/differing" 2>/dev/null; then
  incomplete "could not list the files differing between $UPSTREAM and HEAD."
fi

# NUL-delimited -> newline-delimited, refusing on any path that CONTAINS a
# newline. With core.quotePath=false and -z git emits such a path raw, so a
# line-oriented `comm` would split it and could report a MISSED intersection --
# which is the unsafe direction (a wrong LANDED_DUPLICATE). Pathological, but
# the cost of handling it is one `case` and the cost of not handling it is data
# loss, so it falls through to the patch-id arm instead.
CONTENT_USABLE=1
_nul_to_lines() {
  local src="$1" dst="$2" rec
  : > "$dst"
  while IFS= read -r -d '' rec; do
    [ -n "$rec" ] || continue
    case "$rec" in *$'\n'*) CONTENT_USABLE=0; return 0 ;; esac
    printf '%s\n' "$rec" >> "$dst"
  done < "$src"
}
_nul_to_lines "$TMPD/touched"   "$TMPD/touched.txt"
_nul_to_lines "$TMPD/differing" "$TMPD/differing.txt"

if [ "$CONTENT_USABLE" = 1 ]; then
  LC_ALL=C sort -u "$TMPD/touched.txt"   > "$TMPD/touched.sorted"
  LC_ALL=C sort -u "$TMPD/differing.txt" > "$TMPD/differing.sorted"
  if LC_ALL=C comm -12 "$TMPD/touched.sorted" "$TMPD/differing.sorted" > "$TMPD/overlap" 2>/dev/null; then
    if [ ! -s "$TMPD/overlap" ]; then
      VERDICT="LANDED_DUPLICATE"
      SETTLED_BY="content comparison -- no file this branch touched differs from $UPSTREAM"
      DETAIL=(
        "The ahead-commits ($AHEAD) touch $(wc -l < "$TMPD/touched.sorted" | tr -d ' ') file(s), and every one of them"
        "reads identically on $UPSTREAM. The commits are duplicates under different SHAs,"
        "which is what a rebase-land leaves behind."
        "This checkout is $BEHIND commit(s) behind and holds nothing unique."
      )
      emit_and_exit
    fi
  else
    CONTENT_USABLE=0
  fi
fi

# ---------------------------------------------------------------------------
# 4. PATCH-ID TIEBREAKER -- only reached when the content test was inconclusive.
#
# `git cherry <upstream> HEAD` computes patch-ids for `merge-base..HEAD` and for
# `merge-base..upstream` and marks each HEAD commit `-` (an equivalent exists
# upstream) or `+` (none does). That bounded upstream side is the whole
# difference from the five-minute hand-rolled sweep the plan describes: no date
# floor, no walk of main's whole history.
#
# It IGNORES MERGE COMMITS on both sides, which is why the merge count below is
# load-bearing rather than decorative.
if ! _git cherry -v "$UPSTREAM" HEAD > "$TMPD/cherry" 2>/dev/null; then
  incomplete "git cherry failed comparing HEAD against $UPSTREAM, and the content comparison was inconclusive$( [ "$CONTENT_USABLE" = 0 ] && printf ' (it could not run: a path containing a newline, or comm unavailable)' ). Neither test settled the question."
fi

while IFS= read -r line || [ -n "$line" ]; do
  line="${line%$'\r'}"
  case "$line" in
    '- '*) LANDED+=("${line:2}") ;;
    '+ '*) UNLANDED+=("${line:2}") ;;
  esac
done < "$TMPD/cherry"

N_LANDED=${#LANDED[@]}
N_UNLANDED=${#UNLANDED[@]}
N_CLASSIFIED=$((N_LANDED + N_UNLANDED))

AHEAD_MERGES="$(_git rev-list --count --merges "${UPSTREAM}..HEAD" 2>/dev/null)"
case "${AHEAD_MERGES:-}" in ''|*[!0-9]*) AHEAD_MERGES="" ;; esac

if [ "$N_CLASSIFIED" -eq 0 ]; then
  incomplete "git cherry classified none of the $AHEAD ahead-commit(s)$( [ -n "$AHEAD_MERGES" ] && printf ' (%s of them are merge commits, which patch-id equivalence is undefined for)' "$AHEAD_MERGES" ), and the content comparison was inconclusive. Nothing settled the question."
fi

if [ "$N_UNLANDED" -gt 0 ] && [ "$N_LANDED" -gt 0 ]; then
  VERDICT="MIXED"
  SETTLED_BY="patch-id equivalence over merge-base..$UPSTREAM (git cherry)"
  DETAIL=(
    "$N_LANDED of $N_CLASSIFIED ahead-commit(s) have a patch-equivalent already on $UPSTREAM;"
    "$N_UNLANDED do not. Part of this branch shipped and part did not, so neither"
    "returning it to the default branch nor treating it as untouched peer work is correct."
    "landed:"
  )
  for _c in "${LANDED[@]}";   do DETAIL+=("  - $_c"); done
  DETAIL+=("NOT landed:")
  for _c in "${UNLANDED[@]}"; do DETAIL+=("  + $_c"); done
  emit_and_exit
fi

if [ "$N_LANDED" -eq 0 ]; then
  VERDICT="UNIQUE_WIP"
  SETTLED_BY="patch-id equivalence over merge-base..$UPSTREAM (git cherry)"
  DETAIL=(
    "None of the $N_CLASSIFIED ahead-commit(s) has a patch-equivalent on $UPSTREAM,"
    "and files this branch touches differ from it. This is unshipped content."
  )
  for _c in "${UNLANDED[@]}"; do DETAIL+=("  + $_c"); done
  emit_and_exit
fi

# Every classified commit landed. Three guards stand between here and the one
# verdict that can authorise destroying work.
#
# GUARD 0 -- the merge count itself. Guards A and B are both expressed in terms
# of it, so a merge count that could not be established disarms them silently.
# That is the shape this whole plan is about (a check that exits 0 into
# nothing), and it disarms them on the one arm where a mistake is destructive.
if [ -z "$AHEAD_MERGES" ]; then
  incomplete "all $N_CLASSIFIED classified ahead-commit(s) have landed on $UPSTREAM, but the number of MERGE commits in $UPSTREAM..HEAD could not be counted, so it is unknown whether git cherry skipped any. LANDED_DUPLICATE requires that check to have run."
fi

# GUARD A -- merge commits. `git cherry` skipped them, so "every classified
# commit landed" is not "every ahead-commit landed". An EVIL MERGE (content
# introduced while resolving a conflict) lives in no patch and would be thrown
# away silently. Downgrade to INCOMPLETE.
if [ -n "$AHEAD_MERGES" ] && [ "$AHEAD_MERGES" -gt 0 ]; then
  incomplete "all $N_CLASSIFIED non-merge ahead-commit(s) have landed on $UPSTREAM, but the range also holds $AHEAD_MERGES merge commit(s), which git cherry does not classify. A merge's content is not a patch, so an 'evil merge' carrying conflict-resolution content cannot be ruled out. Reported as INCOMPLETE rather than LANDED_DUPLICATE, which would authorise discarding it."
fi

# GUARD B -- arithmetic. If cherry classified a different number of commits than
# the range contains once merges are excluded, something about this range is not
# what this script models, and the safe answer is to say so.
if [ -n "$AHEAD_MERGES" ] && [ "$N_CLASSIFIED" -ne "$((AHEAD - AHEAD_MERGES))" ]; then
  incomplete "git cherry classified $N_CLASSIFIED commit(s) but the range $UPSTREAM..HEAD holds $AHEAD commit(s) of which $AHEAD_MERGES are merges, so $((AHEAD - AHEAD_MERGES)) were expected. The range is not what this script models; refusing to report LANDED_DUPLICATE on an unexplained mismatch."
fi

VERDICT="LANDED_DUPLICATE"
SETTLED_BY="patch-id equivalence over merge-base..$UPSTREAM (git cherry)"
DETAIL=(
  "All $N_CLASSIFIED ahead-commit(s) have a patch-equivalent commit already on $UPSTREAM."
  "Files this branch touched DO still differ from $UPSTREAM, but only because the"
  "upstream moved on after this branch landed -- the branch itself adds nothing."
  "This checkout is $BEHIND commit(s) behind and holds nothing unique."
)
for _c in "${LANDED[@]}"; do DETAIL+=("  - $_c"); done
emit_and_exit
