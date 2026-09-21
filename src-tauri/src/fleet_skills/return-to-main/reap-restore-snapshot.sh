#!/usr/bin/env bash
# reap-restore-snapshot.sh — the ONE deletion path for a checkout-restore
# snapshot (refs/wip/return-to-main/*) and the parked branch it recovers.
#
# Plan: 2026-09-13-one-recovery-rule-for-both-checkout-restorers, Phase 1 (D4).
# Contract: knowledge-base/qontinui-specific/checkout-restore-contract.md.
#
# Two restorers move a parked primary checkout back to its default branch —
# the ccfg return-to-main sweep and coord/runner RestoreDefault. Both write a
# snapshot ref before the move and KEEP the parked branch. Neither deletes
# anything. Deletion happens here, and only here, when a 14-day retention gate
# clears and its `run_skill return-to-main --reap` continuation runs this
# script on the owning device. So the rule that decides "deleting this loses
# nothing" exists exactly once, and every snapshot, whichever restorer wrote it,
# passes through it.
#
# WHY "MERGED AT HEAD" IS NOT ENOUGH (measured 2026-09-13, git 2.55): a branch
# whose tip landed can still carry commits that only its own REFLOG names — a
# commit the branch was moved to and back from with `git branch -f`, or one
# amended away. `branch -D` deletes that reflog with the ref, and `git fsck`
# then reports the commit unreachable. Check 6 is that measurement. A rebased
# branch's pre-rebase commits are also reflog-only, but they are
# PATCH-EQUIVALENT to landed work (`git cherry` prints `-`); without that
# exclusion every rebased branch would be kept forever.
#
# THE CHECKS, in order (HEAD-shaped snapshot, `…-<sha7>`):
#   1  the running device is --device                      else DEFERRED
#   2  the snapshot ref resolves, its name parses, and its
#      target starts with the name's <sha7>                 else REFUSED
#      (then: `git fetch` origin, and origin/<default> resolves — else UNKNOWN)
#   3  no worktree of this repository has the branch checked out, or is
#      rebasing it (head-name, or listed in a `rebase --update-refs`) or
#      bisecting it (`git worktree list` reads a mid-rebase
#      worktree as `detached`, and `update-ref -d` does not refuse a branch in
#      use, so both are probed here, and probed AGAIN immediately before the
#      delete)
#   4  the branch tip still equals the snapshot sha
#   5  the tip LANDED — any one arm:
#        a  it is an ancestor of origin/<default>;
#        b  no merge commit is in origin/<default>..<tip> (a merge's content is
#           invisible to patch-ids, so an evil merge would pass unexamined),
#           and EVERY commit in that range is VERBATIM-equivalent to one on
#           origin/<default> (see "PATCH EQUIVALENCE" below);
#        c  the snapshot message names `pr #<n>` and land-evidence.sh proves
#           the tip PROVEN_LANDED through a candidate carrying PR <n>
#           (covers a squash land GitHub reports MERGED, and a coord
#           fast-forward land it reports CLOSED)
#   6  the reflog-only set is empty. The set is every commit reachable from a
#      commit `git reflog show refs/heads/<b>` names — its ANCESTORS included,
#      because one reflog entry can jump several commits (`git branch -f`, a
#      fast-forward, `reset --hard`) — that is reachable neither from the tip
#      (check 5's) nor from any ref other than the branch and the snapshot:
#        git rev-list <reflog shas> --not <tip> --exclude=<b> --exclude=<wip> --all
#      Each member must be a non-merge commit VERBATIM-equivalent to one on
#      origin/<default>.
# The branch is never deletable when it is the default branch, is absent, or
# could not be resolved from the snapshot — nor when it was resolved only by a
# tip match and the snapshot carries no writer message at all (a lost reflog
# cannot say an unrelated branch at the same sha is the parked one).
#
# PATCH EQUIVALENCE is verbatim, never `git cherry`'s. A patch-id ignores
# whitespace, so a branch indenting a line OUT of an `if` block is
# "patch-equivalent" to main's version with it inside — measured by the
# pre-PR review. A non-merge commit C counts as landed only when
# `git patch-id --verbatim` of C equals that of some non-merge commit in
# merge-base(C, origin/<default>)..origin/<default>, compared over full diffs
# of the commits touching C's paths, with the diff options pinned on both sides
# (--no-textconv --no-ext-diff --no-renames -U3). A git without `--verbatim`
# (older than 2.39), or ANY step of the comparison failing, makes that UNKNOWN,
# never "not equal" and never a match. An empty commit proves nothing.
#
# OUTCOMES
#   REAPED         checks 1-6 pass: compare-and-delete the branch
#                  (`update-ref -d refs/heads/<b> <sha>`, which refuses if it
#                  moved), remove its `branch.<b>.*` config, compare-and-delete
#                  the snapshot.
#   SNAPSHOT_ONLY  the branch is not deletable (a DECIDED failure of 3-6, or no
#                  branch), but a local branch or origin/<default> still
#                  contains the snapshot sha: the snapshot is redundant, so it
#                  alone is deleted. The branch stays as ordinary local state.
#                  Terminal — nothing to re-gate.
#   REFUSED        nothing deleted, and the snapshot is the only holder of its
#                  commit (or it did not parse, or a compare-and-delete
#                  refused). Reconciliation re-gates it for another 14 days.
#   UNKNOWN        a probe that could decide the outcome did not run (fetch
#                  failed, no origin/<default>, land-evidence unavailable while
#                  it was the only arm left, no device identity). Nothing
#                  deleted; re-gated.
#   DEFERRED       wrong device: another device's snapshot. Nothing read beyond
#                  the device id, nothing deleted; the owning device's
#                  reconciliation re-gates it.
#   ABSENT         the snapshot ref no longer exists; nothing to retain.
#
# THE RESIDUE SHAPE (`…-residue`, a `git stash create` commit the sweep writes
# before a residue restore — contract item 6). Deleted when every one holds,
# otherwise REFUSED (UNKNOWN when a probe could not run):
#   * exactly 2 parents, and R^2^{tree} == R^1^{tree} (tracked `.M` edits only);
#   * R^1 (the parked HEAD it was taken on) is still contained in a local branch
#     or origin/<default> — otherwise the snapshot is the only ref holding it;
#   * every path p in `git diff --name-only R^1 R` has a blob B = R:p that
#     passes one arm of dirty-provenance.sh's own admission test:
#       UPSTREAM_HISTORICAL  `git log origin/<default> -1 --find-object=B -- p`
#       EOL_ONLY             R^1:p and B differ only by carriage returns
#       RUNNER_BUNDLE        B occurs at the bundle path in qontinui-runner's
#                            origin/<default> history
#     The running-runner-BUILD arm cannot be re-checked later, so a file that
#     matched only there refuses. An empty diff passes.
#
# WRITES — the complete set (grep `gw `): `git fetch` into refs/remotes/origin/*
# only (explicit refspec + empty --refmap=, as return-to-main-sweep.sh), and on
# the delete arms `update-ref -d <ref> <old>` and `config --remove-section`.
#
# USAGE
#   reap-restore-snapshot.sh <repo-dir> <wip_ref> --device <device_id>
#                            [--log <file>] [--fetch-timeout <sec>]
#   --device <id>       the owning device, from the retention gate's continuation.
#   --log <file>        also append the decision line to <file>.
#   --fetch-timeout N   seconds for the fetch (default 120).
# Output: exactly one JSON line on stdout (the decision), a one-line summary on
# stderr. The line carries `outcome`, `work_outcome` (`work_completed` or
# `work_abandoned: <reason>`), `deleted[]`, `checks[]` and the parsed snapshot.
# ENV  QONTINUI_MACHINE_ID (else ~/.qontinui/machine.json `device_id`),
#      REAP_LIB_DIR, REAP_LAND_EVIDENCE, QONTINUI_RUNNER_REPO (the residue
#      bundle arm; default the repo-dir's sibling qontinui-runner/),
#      REAP_RACE_HOOK + REAP_TEST_SEAMS=1 (TEST SEAM, honoured only with both:
#      a command run immediately before the first delete, so the suite can move
#      a ref between check and delete).
#
# EXIT
#   0  REAPED
#   1  SNAPSHOT_ONLY
#   2  REFUSED
#   3  UNKNOWN
#   4  usage
#   5  DEFERRED
#   6  ABSENT
# ---- END HELP

set -u

_rr_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -n "${REAP_LIB_DIR:-}" ]; then LIB_DIR="$REAP_LIB_DIR"
elif [ -d "$_rr_dir/lib" ]; then LIB_DIR="$_rr_dir/lib"
else LIB_DIR="$_rr_dir/../../../scripts/lib"; fi
if [ -n "${REAP_LAND_EVIDENCE:-}" ]; then LAND_EVIDENCE="$REAP_LAND_EVIDENCE"
elif [ -f "$_rr_dir/land-evidence.sh" ]; then LAND_EVIDENCE="$_rr_dir/land-evidence.sh"
else LAND_EVIDENCE="$_rr_dir/../../../scripts/land-evidence.sh"; fi

if [ -r "$LIB_DIR/git-scope.sh" ]; then . "$LIB_DIR/git-scope.sh"; fi
if declare -F git_scope_strip >/dev/null 2>&1; then
  git_scope_strip
elif [ -n "${GIT_DIR+s}${GIT_WORK_TREE+s}${GIT_COMMON_DIR+s}" ]; then
  echo "reap-restore-snapshot: FATAL - lib/git-scope.sh is not usable ($LIB_DIR) AND GIT_DIR/GIT_WORK_TREE/GIT_COMMON_DIR is set; every answer would be about another repository. Refusing." >&2
  exit 4
fi
if [ -r "$LIB_DIR/native-path.sh" ]; then . "$LIB_DIR/native-path.sh"; fi
if ! declare -F native_path_w >/dev/null 2>&1; then
  if command -v cygpath >/dev/null 2>&1; then
    echo "reap-restore-snapshot: FATAL - lib/native-path.sh is not usable ($LIB_DIR) on an MSYS box. Refusing." >&2
    exit 4
  fi
  native_path_w() { printf '%s\n' "$1"; }
fi
# `<rev>:<path>` revspecs and `refs/...` names are what MSYS mangles.
export MSYS_NO_PATHCONV=1

usage() { sed -n '2,/^# ---- END HELP/p' "$0" | sed '$d' | sed 's/^#\{0,1\} \{0,1\}//'; }
usage_err() { echo "reap-restore-snapshot: $1 (see --help)" >&2; exit 4; }

REPO_DIR=""; WIP_REF=""; DEVICE=""; LOG=""; FETCH_TIMEOUT=120
while [ $# -gt 0 ]; do
  case "$1" in
    --device|--log|--fetch-timeout)
      [ $# -ge 2 ] && [ -n "$2" ] || usage_err "$1 needs a value"
      case "$1" in --device) DEVICE="$2" ;; --log) LOG="$2" ;; *) FETCH_TIMEOUT="$2" ;; esac
      shift 2 ;;
    -h|--help) usage; exit 0 ;;
    -*) usage_err "unknown option $1" ;;
    *) if [ -z "$REPO_DIR" ]; then REPO_DIR="$1"
       elif [ -z "$WIP_REF" ]; then WIP_REF="$1"
       else usage_err "unexpected argument $1"; fi
       shift ;;
  esac
done
[ -n "$REPO_DIR" ] && [ -n "$WIP_REF" ] || usage_err "<repo-dir> and <wip_ref> are required"
[ -n "$DEVICE" ] || usage_err "--device is required"
case "$FETCH_TIMEOUT" in ''|*[!0-9]*|0) usage_err "--fetch-timeout needs a positive number of seconds" ;; esac
[ -d "$REPO_DIR" ] || usage_err "no such directory: $REPO_DIR"
REPO_DIR="$(cd "$REPO_DIR" && pwd)"
CO_N="$(native_path_w "$REPO_DIR")"

g()  { git --no-optional-locks -c core.quotePath=false -C "$CO_N" "$@"; }
gw() { git -C "$CO_N" "$@"; }

g rev-parse --git-dir >/dev/null 2>&1 || usage_err "not a git checkout: $REPO_DIR"

TMP="$(mktemp -d)" || { echo "reap-restore-snapshot: cannot mktemp -d" >&2; exit 3; }
trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# JSON
json_escape() {
  local s="$1"
  s="${s//\\/\\\\}"; s="${s//\"/\\\"}"; s="${s//$'\n'/\\n}"; s="${s//$'\r'/\\r}"; s="${s//$'\t'/\\t}"
  printf '%s' "$s" | tr -d '\000-\010\013\014\016-\037'
}
js()  { printf '"%s"' "$(json_escape "$1")"; }
jsn() { if [ -n "${1:-}" ]; then js "$1"; else printf 'null'; fi; }

CHECKS=""
check() { # <id> <result pass|fail|unknown|skipped> <detail>
  [ -n "$CHECKS" ] && CHECKS="$CHECKS,"
  CHECKS="$CHECKS{\"check\":$(js "$1"),\"result\":$(js "$2"),\"detail\":$(js "$3")}"
}
DELETED=()
SHAPE=""; STAMP=""; NAME_REPO=""; NAME_SHA=""; SNAP_SHA=""; WRITER=""; MESSAGE=""
BRANCH=""; BRANCH_SOURCE=""; PR_NUM=""; DEFAULT_REF=""; FETCHED=false; FETCH_ERR=""
RUNNING_DEVICE=""

finish() { # <outcome> <reason>
  local outcome="$1" reason="$2" rc work d dj="" line
  case "$outcome" in
    REAPED) rc=0; work="work_completed" ;;
    SNAPSHOT_ONLY) rc=1; work="work_completed" ;;
    REFUSED) rc=2; work="work_abandoned: refused: $reason" ;;
    UNKNOWN) rc=3; work="work_abandoned: unknown: $reason" ;;
    DEFERRED) rc=5; work="work_abandoned: wrong_device" ;;
    ABSENT) rc=6; work="work_completed" ;;
    *) rc=3; work="work_abandoned: unknown: internal outcome $outcome" ;;
  esac
  for d in ${DELETED[@]+"${DELETED[@]}"}; do dj="${dj:+$dj,}$(js "$d")"; done
  line="{\"ts\":$(js "$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null)"),\"event\":\"reap_decision\",\"tool\":\"reap-restore-snapshot.sh\",\"schema\":1,\"outcome\":$(js "$outcome"),\"work_outcome\":$(js "$work"),\"reason\":$(js "$reason"),\"device\":$(js "$DEVICE"),\"running_device\":$(jsn "$RUNNING_DEVICE"),\"checkout\":$(js "$REPO_DIR"),\"wip_ref\":$(js "$WIP_REF"),\"shape\":$(jsn "$SHAPE"),\"stamp\":$(jsn "$STAMP"),\"name_repo\":$(jsn "$NAME_REPO"),\"snapshot_sha\":$(jsn "$SNAP_SHA"),\"writer\":$(jsn "$WRITER"),\"message\":$(jsn "$MESSAGE"),\"branch\":$(jsn "$BRANCH"),\"branch_source\":$(jsn "$BRANCH_SOURCE"),\"pr\":$(jsn "$PR_NUM"),\"default_ref\":$(jsn "$DEFAULT_REF"),\"fetched\":$FETCHED,\"fetch_error\":$(jsn "$FETCH_ERR"),\"deleted\":[${dj}],\"checks\":[${CHECKS}]}"
  printf '%s\n' "$line"
  [ -n "$LOG" ] && printf '%s\n' "$line" >>"$LOG" 2>/dev/null
  echo "reap-restore-snapshot: $outcome $WIP_REF -- $reason" >&2
  exit "$rc"
}

# ---------------------------------------------------------------------------
# Check 1: the device. Read before anything else about the repository: a
# continuation that coord re-targeted to the wrong machine touches nothing.
RUNNING_DEVICE="${QONTINUI_MACHINE_ID:-}"
if [ -z "$RUNNING_DEVICE" ]; then
  _home="${HOME:-${USERPROFILE:-}}"
  if [ -n "$_home" ] && [ -r "$_home/.qontinui/machine.json" ]; then
    RUNNING_DEVICE="$(grep -o '"\(device_id\|machine_id\)"[[:space:]]*:[[:space:]]*"[^"]*"' "$_home/.qontinui/machine.json" | head -1 | sed 's/.*:[[:space:]]*"//; s/"$//')"
  fi
fi
if [ -z "$RUNNING_DEVICE" ]; then
  check 1_device unknown "no device id: QONTINUI_MACHINE_ID is unset and ~/.qontinui/machine.json has no device_id"
  finish UNKNOWN "the running device could not be identified"
fi
if [ "$(printf '%s' "$RUNNING_DEVICE" | tr 'A-F' 'a-f')" != "$(printf '%s' "$DEVICE" | tr 'A-F' 'a-f')" ]; then
  check 1_device fail "running device $RUNNING_DEVICE is not the owning device $DEVICE"
  finish DEFERRED "wrong_device: this snapshot belongs to device $DEVICE"
fi
check 1_device pass "running device $RUNNING_DEVICE"

# ---------------------------------------------------------------------------
# Check 2: the snapshot resolves and parses.
case "$WIP_REF" in
  refs/wip/return-to-main/*) ;;
  *) check 2_snapshot fail "not a refs/wip/return-to-main/ ref"; finish REFUSED "unparseable snapshot name (not under refs/wip/return-to-main/)" ;;
esac
if ! g check-ref-format "$WIP_REF" >/dev/null 2>&1; then
  check 2_snapshot fail "git check-ref-format rejects it"; finish REFUSED "unparseable snapshot name"
fi
if ! g show-ref --verify --quiet "$WIP_REF"; then
  check 2_snapshot skipped "the ref does not exist"
  finish ABSENT "the snapshot ref is already gone"
fi
SNAP_SHA="$(g rev-parse --verify --quiet "$WIP_REF^{commit}")"
_leaf="${WIP_REF#refs/wip/return-to-main/}"
if [[ "$_leaf" =~ ^([0-9]{8}T[0-9]{6}Z)-(.+)-(residue|[0-9a-f]{7,40})$ ]]; then
  STAMP="${BASH_REMATCH[1]}"; NAME_REPO="${BASH_REMATCH[2]}"
  if [ "${BASH_REMATCH[3]}" = residue ]; then SHAPE=residue; else SHAPE=head; NAME_SHA="${BASH_REMATCH[3]}"; fi
else
  check 2_snapshot fail "leaf '$_leaf' is not <UTCstamp>-<repo>-<sha7|residue>"
  finish REFUSED "unparseable snapshot name"
fi
if [ -z "$SNAP_SHA" ]; then
  check 2_snapshot fail "the ref does not resolve to a commit"; finish REFUSED "snapshot does not resolve to a commit"
fi
if [ "$SHAPE" = head ] && [ "${SNAP_SHA:0:${#NAME_SHA}}" != "$NAME_SHA" ]; then
  check 2_snapshot fail "the ref points at $SNAP_SHA, not the recorded $NAME_SHA"
  finish REFUSED "snapshot ref moved off its recorded sha $NAME_SHA"
fi
# The writer's message is the OLDEST reflog entry (the creation).
MESSAGE="$(g log -g --format=%gs "$WIP_REF" -- 2>/dev/null | tail -1 | tr -d '\r')"
WRITER="${MESSAGE%%:*}"; [ "$WRITER" = "$MESSAGE" ] && WRITER=""
_reposafe="$(printf '%s' "$(basename "$REPO_DIR")" | tr -c 'A-Za-z0-9._-' '_')"
if [ "$NAME_REPO" != "$_reposafe" ]; then
  check 2_snapshot fail "the name says repo '$NAME_REPO' but it lives in '$(basename "$REPO_DIR")'"
  finish REFUSED "snapshot name names another repository"
fi
check 2_snapshot pass "$SHAPE snapshot -> $SNAP_SHA${WRITER:+ (writer $WRITER)}"

# ---------------------------------------------------------------------------
# The fetch, then origin/<default>. Every land decision reads the remote as of
# now, never as of whenever this checkout was last fetched.
_timeout() {
  local secs="$1"; shift
  if command -v timeout >/dev/null 2>&1; then timeout "$secs" "$@"; return $?; fi
  if command -v gtimeout >/dev/null 2>&1; then gtimeout "$secs" "$@"; return $?; fi
  "$@"
}
_out="$(GIT_TERMINAL_PROMPT=0 GCM_INTERACTIVE=never \
        _timeout "$FETCH_TIMEOUT" git -C "$CO_N" -c credential.interactive=never \
          fetch --quiet --prune --no-recurse-submodules --refmap= origin \
          '+refs/heads/*:refs/remotes/origin/*' 2>&1)"; _rc=$?
if [ "$_rc" -ne 0 ]; then
  FETCH_ERR="git fetch exited $_rc: ${_out:-<no output>}"; FETCH_ERR="${FETCH_ERR:0:400}"
  check fetch unknown "$FETCH_ERR"
  finish UNKNOWN "fetch failed, so no land decision can be made"
fi
FETCHED=true
_r="$(g symbolic-ref -q refs/remotes/origin/HEAD 2>/dev/null)"
if [ -n "$_r" ] && g rev-parse -q --verify "$_r^{commit}" >/dev/null 2>&1; then
  DEFAULT_REF="${_r#refs/remotes/}"
else
  for _r in origin/main origin/master; do
    g rev-parse -q --verify "refs/remotes/$_r^{commit}" >/dev/null 2>&1 && { DEFAULT_REF="$_r"; break; }
  done
fi
if [ -z "$DEFAULT_REF" ]; then
  check fetch unknown "fetched, but no origin/HEAD, origin/main or origin/master resolves"
  finish UNKNOWN "no origin/<default> to judge a land against"
fi
DEFAULT_BRANCH="${DEFAULT_REF#origin/}"
check fetch pass "fetched; default ref $DEFAULT_REF"

delete_snapshot() { # -> 0 deleted, 1 compare-and-delete refused
  if gw update-ref -d "$WIP_REF" "$SNAP_SHA" 2>"$TMP/del.err"; then DELETED+=("$WIP_REF"); return 0; fi
  return 1
}
race_hook() { [ "${REAP_TEST_SEAMS:-}" = 1 ] && [ -n "${REAP_RACE_HOOK:-}" ] && bash -c "$REAP_RACE_HOOK"; return 0; }

# verbatim_landed <commit> -> 0 landed verbatim, 1 not, 2 cannot be established.
PATCHID_VERBATIM=""
printf '' | git patch-id --verbatim >/dev/null 2>&1 && PATCHID_VERBATIM=1
# Every step's failure is 2 (UNKNOWN), never 1: a "not equal" that is really
# "could not compare" would end the retention cycle as a decided SNAPSHOT_ONLY.
# The diff options are pinned on BOTH sides, because patch-ids are only as
# honest as the diff they hash: --no-textconv (a lossy textconv driver can hide
# a real difference), --no-ext-diff, --no-renames, and -U3 (a patch-id ignores
# `@@` line numbers, so a zero-context diff would let one hunk stand in for
# another elsewhere in the file).
VERBATIM_DIFF_OPTS=(--no-color --no-ext-diff --no-textconv --no-renames -U3)
verbatim_landed() {
  local c="$1" parents id base pth
  [ -n "$PATCHID_VERBATIM" ] || return 2
  parents="$(g rev-list --parents -n1 "$c" 2>/dev/null)" || return 2
  [ "$(printf '%s' "$parents" | wc -w | tr -d ' ')" = 2 ] || return 1
  g show "${VERBATIM_DIFF_OPTS[@]}" --format='commit %H' "$c" >"$TMP/vc" 2>/dev/null || return 2
  git patch-id --verbatim <"$TMP/vc" >"$TMP/vc.id" 2>/dev/null || return 2
  id="$(awk 'NR == 1 { print $1 }' "$TMP/vc.id")"
  if [ -z "$id" ]; then
    grep -q '^diff --git ' "$TMP/vc" && return 2   # a diff patch-id could not hash
    return 1                                      # an empty commit proves nothing
  fi
  base="$(g merge-base "$c" "$DEFAULT_REF" 2>/dev/null)" || return 2
  g diff-tree --no-commit-id --no-renames --name-only -r -z "$c" >"$TMP/vc.paths" 2>/dev/null || return 2
  local paths=()
  while IFS= read -r -d '' pth; do paths+=(":(literal)$pth"); done <"$TMP/vc.paths"
  [ "${#paths[@]}" -gt 0 ] || return 2
  g log --no-merges "${VERBATIM_DIFF_OPTS[@]}" -p --full-diff --format='commit %H' "$base..$DEFAULT_REF" -- "${paths[@]}" >"$TMP/vl" 2>/dev/null || return 2
  git patch-id --verbatim <"$TMP/vl" >"$TMP/vl.id" 2>/dev/null || return 2
  awk -v id="$id" '$1 == id { f = 1 } END { exit f ? 0 : 1 }' "$TMP/vl.id"
}

# branch_busy -> 0 free, 1 busy (BUSY_WHY), 2 unknown (BUSY_WHY). Checked out in
# any worktree, or mid-rebase / mid-bisect on it in any worktree.
branch_busy() {
  local common d f v
  BUSY_WHY=""
  if ! g worktree list --porcelain >"$TMP/wt" 2>/dev/null; then BUSY_WHY="git worktree list failed"; return 2; fi
  if tr -d '\r' <"$TMP/wt" | grep -qxF "branch refs/heads/$BRANCH"; then
    BUSY_WHY="$BRANCH is checked out again (someone undid the restore)"; return 1
  fi
  common="$(g rev-parse --path-format=absolute --git-common-dir 2>/dev/null | tr -d '\r')"
  [ -n "$common" ] && [ -d "$common" ] || { BUSY_WHY="the git common dir did not resolve"; return 2; }
  for d in "$common" "$common"/worktrees/*; do
    [ -d "$d" ] || continue
    for f in rebase-merge/head-name rebase-apply/head-name BISECT_START rebase-merge/update-refs; do
      [ -e "$d/$f" ] || continue
      [ -r "$d/$f" ] || { BUSY_WHY="$d/$f exists but is unreadable"; return 2; }
      if [ "$f" = rebase-merge/update-refs ]; then
        # `rebase --update-refs`: records of three lines, the first the ref name
        # the rebase will move at its end. git itself treats it as in use.
        if tr -d '\r' <"$d/$f" | grep -qxF "refs/heads/$BRANCH"; then
          BUSY_WHY="$BRANCH is listed in a rebase --update-refs in $(basename "$d")"; return 1
        fi
        continue
      fi
      v="$(tr -d '\r\n' <"$d/$f")"
      if [ "$v" = "refs/heads/$BRANCH" ] || { [ "$f" = BISECT_START ] && [ "$v" = "$BRANCH" ]; }; then
        BUSY_WHY="$BRANCH is mid-$( [ "$f" = BISECT_START ] && echo bisect || echo rebase ) in $(basename "$d")"; return 1
      fi
    done
  done
  return 0
}

# ---------------------------------------------------------------------------
# THE RESIDUE SHAPE
if [ "$SHAPE" = residue ]; then
  _parents="$(g rev-list --parents -n1 "$SNAP_SHA" | wc -w | tr -d ' ')"
  if [ "$_parents" != 3 ]; then
    check residue_shape fail "commit has $((_parents - 1)) parent(s), not 2"
    finish REFUSED "residue snapshot is not a two-parent stash commit"
  fi
  if [ "$(g rev-parse "$SNAP_SHA^2^{tree}")" != "$(g rev-parse "$SNAP_SHA^1^{tree}")" ]; then
    check residue_shape fail "R^2^{tree} differs from R^1^{tree}: the stash carries staged changes"
    finish REFUSED "residue snapshot carries staged changes"
  fi
  check residue_shape pass "two parents, index tree equals HEAD tree"

  RUNNER_REPO="${QONTINUI_RUNNER_REPO:-$(dirname "$REPO_DIR")/qontinui-runner}"
  RUNNER_N=""; RUNNER_UP=""
  if [ -d "$RUNNER_REPO" ]; then
    RUNNER_N="$(native_path_w "$RUNNER_REPO")"
    for _r in origin/main origin/master; do
      git --no-optional-locks -C "$RUNNER_N" rev-parse -q --verify "refs/remotes/$_r^{commit}" >/dev/null 2>&1 && { RUNNER_UP="$_r"; break; }
    done
  fi
  bundle_path_for() {
    case "$1" in
      .claude/commands/*) printf 'src-tauri/src/fleet_commands/%s' "${1#.claude/commands/}" ;;
      .claude/skills/*)   printf 'src-tauri/src/fleet_skills/%s'   "${1#.claude/skills/}" ;;
      *) return 1 ;;
    esac
  }
  hash_nocr() { tr -d '\r' | cksum; }

  if ! g diff --name-only -z "$SNAP_SHA^1" "$SNAP_SHA" >"$TMP/paths" 2>/dev/null; then
    check residue_blobs unknown "git diff R^1 R failed"; finish UNKNOWN "residue path list unreadable"
  fi
  _n=0; _undecided=""
  while IFS= read -r -d '' p; do
    _n=$((_n + 1))
    B="$(g rev-parse -q --verify "$SNAP_SHA:$p" 2>/dev/null)"
    if [ -z "$B" ]; then
      check "residue:$p" fail "no blob at R:$p (a deletion), which no provisioner produces"
      finish REFUSED "residue snapshot deletes $p"
    fi
    _h="$(g log "$DEFAULT_REF" -1 --format=%h --find-object="$B" -- ":(literal)$p" 2>/dev/null)"; _lrc=$?
    if [ "$_lrc" -eq 0 ] && [ -n "$_h" ]; then
      check "residue:$p" pass "UPSTREAM_HISTORICAL: blob ${B:0:12} occurs in $DEFAULT_REF history (commit $_h)"; continue
    fi
    [ "$_lrc" -ne 0 ] && _undecided="history read on $DEFAULT_REF failed"
    if g cat-file -e "$SNAP_SHA^1:$p" 2>/dev/null \
       && [ "$(g cat-file blob "$B" | hash_nocr)" = "$(g cat-file blob "$SNAP_SHA^1:$p" | hash_nocr)" ]; then
      check "residue:$p" pass "EOL_ONLY: differs from R^1 only by carriage returns"; continue
    fi
    if bp="$(bundle_path_for "$p")"; then
      if [ -n "$RUNNER_UP" ]; then
        _h="$(git --no-optional-locks -C "$RUNNER_N" log "$RUNNER_UP" -1 --format=%h --find-object="$B" -- ":(literal)$bp" 2>/dev/null)"
        if [ -n "$_h" ]; then
          check "residue:$p" pass "RUNNER_BUNDLE: blob ${B:0:12} occurs at $bp in runner $RUNNER_UP history (commit $_h)"; continue
        fi
      else
        _undecided="${_undecided:+$_undecided; }no qontinui-runner checkout with an origin/main at $RUNNER_REPO"
      fi
    fi
    if [ -n "$_undecided" ]; then
      check "residue:$p" unknown "not residue by any arm that ran; undecided because: $_undecided"
      finish UNKNOWN "residue provenance of $p could not be decided"
    fi
    check "residue:$p" fail "blob ${B:0:12} is in no upstream history, is not EOL-only, and no runner-history arm holds it (a match only against a runner BUILD cannot be re-checked)"
    finish REFUSED "residue snapshot holds content that is not provable residue: $p"
  done <"$TMP/paths"
  check residue_blobs pass "$_n path(s), every blob is provable residue"
  _base="$(g rev-parse --verify --quiet "$SNAP_SHA^1^{commit}")"
  _base_holder=""
  if g merge-base --is-ancestor "$_base" "$DEFAULT_REF" 2>/dev/null; then _base_holder="$DEFAULT_REF"
  else
    while IFS= read -r _b; do
      [ -n "$_b" ] && g merge-base --is-ancestor "$_base" "refs/heads/$_b" 2>/dev/null && { _base_holder="refs/heads/$_b"; break; }
    done < <(g for-each-ref --format='%(refname:strip=2)' refs/heads | tr -d '\r')
  fi
  if [ -z "$_base_holder" ]; then
    check residue_base fail "no local branch and not $DEFAULT_REF contains R^1 $_base: the snapshot is the only ref holding it"
    finish REFUSED "residue snapshot is the only ref holding its base commit $_base"
  fi
  check residue_base pass "$_base_holder contains R^1 $_base"
  race_hook
  if delete_snapshot; then finish REAPED "residue snapshot holds only provable residue"; fi
  check delete fail "compare-and-delete of $WIP_REF refused: $(head -1 "$TMP/del.err")"
  finish REFUSED "the snapshot moved between check and delete"
fi

# ---------------------------------------------------------------------------
# THE HEAD SHAPE — resolve the parked branch.
PR_NUM="$(printf '%s' "$MESSAGE" | grep -oiE '\bpr #[0-9]+' | head -1 | grep -oE '[0-9]+')"
if [[ "$MESSAGE" =~ leaving[[:space:]]([^[:space:]]+)[[:space:]]\( ]]; then
  BRANCH="${BASH_REMATCH[1]}"; BRANCH_SOURCE=message
  if [ "$BRANCH" = "<detached>" ]; then BRANCH=""; BRANCH_SOURCE=detached; fi
else
  _tips="$(g for-each-ref --format='%(objectname) %(refname:strip=2)' refs/heads | awk -v s="$SNAP_SHA" '$1 == s { print $2 }')"
  case "$(printf '%s' "$_tips" | grep -c .)" in
    1) BRANCH="$_tips"; BRANCH_SOURCE=tip_match ;;
    0) BRANCH_SOURCE=unresolved_no_tip_match ;;
    *) BRANCH_SOURCE=unresolved_ambiguous_tip_match ;;
  esac
fi

UNKNOWN_WHY=""      # a check that could not run
NOT_DELETABLE=""    # the first DECIDED reason the branch stays

if [ -z "$BRANCH" ]; then
  NOT_DELETABLE="no parked branch resolves ($BRANCH_SOURCE)"
  check 3_not_checked_out skipped "$NOT_DELETABLE"
elif [ "$BRANCH" = "$DEFAULT_BRANCH" ]; then
  NOT_DELETABLE="the parked branch is the default branch $DEFAULT_BRANCH (a fast-forward snapshot), which is never deleted"
  check 3_not_checked_out skipped "$NOT_DELETABLE"
elif ! g show-ref --verify --quiet "refs/heads/$BRANCH"; then
  NOT_DELETABLE="refs/heads/$BRANCH no longer exists"
  check 3_not_checked_out skipped "$NOT_DELETABLE"
elif [ "$BRANCH_SOURCE" = tip_match ] && [ -z "$MESSAGE" ]; then
  NOT_DELETABLE="$BRANCH was resolved only by a tip match and the snapshot carries no writer message, so nothing says it is the parked branch"
  check 3_not_checked_out skipped "$NOT_DELETABLE"
fi

if [ -z "$NOT_DELETABLE" ]; then
  # Check 3: no worktree (the main one included) has the branch checked out,
  # rebasing it, or bisecting it.
  branch_busy; _bb=$?
  if [ "$_bb" = 2 ]; then
    UNKNOWN_WHY="$BUSY_WHY"; check 3_not_checked_out unknown "$UNKNOWN_WHY"
  elif [ "$_bb" = 1 ]; then
    NOT_DELETABLE="$BUSY_WHY"; check 3_not_checked_out fail "$NOT_DELETABLE"
  else
    check 3_not_checked_out pass "no worktree has $BRANCH checked out, rebasing or bisecting"
  fi
fi

TIP=""
if [ -z "$NOT_DELETABLE" ] && [ -z "$UNKNOWN_WHY" ]; then
  # Check 4
  TIP="$(g rev-parse --verify --quiet "refs/heads/$BRANCH^{commit}")"
  if [ "$TIP" != "$SNAP_SHA" ]; then
    NOT_DELETABLE="$BRANCH moved since the snapshot (tip ${TIP:-?}, snapshot $SNAP_SHA): live work"
    check 4_tip_unchanged fail "$NOT_DELETABLE"
  else
    check 4_tip_unchanged pass "$BRANCH tip is still $SNAP_SHA"
  fi
fi

if [ -z "$NOT_DELETABLE" ] && [ -z "$UNKNOWN_WHY" ]; then
  # Check 5
  LANDED=""
  if g merge-base --is-ancestor "$TIP" "$DEFAULT_REF" 2>/dev/null; then
    LANDED="5a: an ancestor of $DEFAULT_REF"
  else
    _cherry="$(g cherry "$DEFAULT_REF" "$TIP" 2>/dev/null)"; _crc=$?
    _merges="$(g rev-list --count --merges "$DEFAULT_REF..$TIP" 2>/dev/null)"; _mrc=$?
    if [ "$_crc" -ne 0 ] || [ "$_mrc" -ne 0 ]; then
      _arm_b_unknown="git cherry / rev-list against $DEFAULT_REF failed"
    elif [ -n "$_cherry" ] && ! printf '%s\n' "$_cherry" | grep -q '^+' && [ "$_merges" = 0 ]; then
      # `git cherry` is only the cheap filter: its patch-ids ignore whitespace.
      # The range is read into a file and its size must equal cherry's line
      # count: a failed or short read must never leave "no commit disagreed"
      # standing in for "every commit was compared".
      _vb=unknown; _checked=0
      _want="$(printf '%s\n' "$_cherry" | grep -c .)"
      if ! g rev-list "$DEFAULT_REF..$TIP" >"$TMP/5b" 2>/dev/null; then
        _arm_b_unknown="rev-list $DEFAULT_REF..$TIP failed"
      else
        _vb=ok
        for c in $(tr -d '\r' <"$TMP/5b"); do
          verbatim_landed "$c"; _vr=$?
          if [ "$_vr" = 0 ]; then _checked=$((_checked + 1)); continue; fi
          if [ "$_vr" = 2 ]; then _vb=unknown; _arm_b_unknown="verbatim patch-id for $c could not be established$([ -n "$PATCHID_VERBATIM" ] || printf " (this git has no patch-id --verbatim)")"
          else _vb=differs; fi
          break
        done
        if [ "$_vb" = ok ] && { [ "$_checked" -lt 1 ] || [ "$_checked" != "$_want" ]; }; then
          _vb=unknown; _arm_b_unknown="5b compared $_checked commit(s) but git cherry listed $_want"
        fi
      fi
      [ "$_vb" = ok ] && LANDED="5b: every commit in $DEFAULT_REF..$TIP is verbatim-equivalent to one on $DEFAULT_REF, and none is a merge"
    fi
    if [ -z "$LANDED" ] && [ -n "$PR_NUM" ]; then
      if [ ! -r "$LAND_EVIDENCE" ]; then
        UNKNOWN_WHY="the snapshot names PR #$PR_NUM but land-evidence.sh is not readable at $LAND_EVIDENCE"
      else
        bash "$LAND_EVIDENCE" --json --ref "$TIP" --branch "$BRANCH" --upstream "$DEFAULT_REF" "$REPO_DIR" >"$TMP/le.json" 2>"$TMP/le.err"; _lerc=$?
        _le="$(tr -d '\r\n' <"$TMP/le.json")"
        if [ "$_lerc" -eq 0 ] && printf '%s' "$_le" | grep -q '"evidence_strength":"PROVEN_LANDED"' \
           && printf '%s' "$_le" | grep -qE "\"pr\":\"?$PR_NUM\"?[,}]"; then
          LANDED="5c: land-evidence.sh proves $TIP PROVEN_LANDED through PR #$PR_NUM"
        elif [ "$_lerc" -eq 5 ]; then
          # Exit 5 is SUPERSEDED: upstream moved PAST this branch, which is not a
          # land and must never be read as one here -- 5c is what lets this
          # reaper DELETE a snapshot and its parked branch. It already landed in
          # the `-ge 4` catch-all below and so already read UNKNOWN, i.e. the
          # safe direction BY ACCIDENT, with a message naming no cause. The
          # verdict is unchanged and now deliberate; the reason says why.
          # (plan 2026-09-16-land-evidence-has-no-superseded-arm, Phase 6.)
          UNKNOWN_WHY="land-evidence.sh reports $TIP SUPERSEDED (exit 5) rather than landed through PR #$PR_NUM: upstream moved past this branch, which is not evidence its content reached $DEFAULT_REF. 5c is not satisfied"  # skill-self-path-ok: message text; the invocation is $LAND_EVIDENCE, resolved from $_rr_dir above
        elif [ "$_lerc" -eq 3 ] || [ "$_lerc" -ge 4 ]; then
          UNKNOWN_WHY="land-evidence.sh for PR #$PR_NUM exited $_lerc: $(head -1 "$TMP/le.err" | cut -c1-200)"  # skill-self-path-ok: message text; the invocation is $LAND_EVIDENCE, resolved from $_rr_dir above
        fi
      fi
    fi
    [ -z "$LANDED" ] && [ -z "$UNKNOWN_WHY" ] && [ -n "${_arm_b_unknown:-}" ] && UNKNOWN_WHY="$_arm_b_unknown"
  fi
  if [ -n "$LANDED" ]; then
    check 5_landed pass "$LANDED"
  elif [ -n "$UNKNOWN_WHY" ]; then
    check 5_landed unknown "$UNKNOWN_WHY"
  else
    NOT_DELETABLE="$TIP has not landed on $DEFAULT_REF (not an ancestor, not patch-equivalent${PR_NUM:+, PR #$PR_NUM not proven})"
    check 5_landed fail "$NOT_DELETABLE"
  fi
fi

if [ -z "$NOT_DELETABLE" ] && [ -z "$UNKNOWN_WHY" ]; then
  # Check 6: the reflog-only set.
  if ! g log -g --format=%H "refs/heads/$BRANCH" -- >"$TMP/reflog" 2>/dev/null; then
    UNKNOWN_WHY="the reflog of refs/heads/$BRANCH could not be read"; check 6_reflog_only_empty unknown "$UNKNOWN_WHY"
  else
    _unique=""; _n=0; _equiv=0; _named=()
    for c in $(tr -d '\r' <"$TMP/reflog" | awk 'NF && !seen[$0]++'); do
      g cat-file -e "$c^{commit}" 2>/dev/null && _named+=("$c")   # a pruned entry leaves nothing to lose
    done
    : >"$TMP/only"
    if [ "${#_named[@]}" -gt 0 ] \
       && ! g rev-list "${_named[@]}" --not "$TIP" --exclude="refs/heads/$BRANCH" --exclude="$WIP_REF" --all >"$TMP/only" 2>/dev/null; then
      UNKNOWN_WHY="rev-list of the reflog-only set of refs/heads/$BRANCH failed"
    fi
    if [ -z "$UNKNOWN_WHY" ]; then
      for c in $(tr -d '\r' <"$TMP/only"); do
        _n=$((_n + 1))
        verbatim_landed "$c"; _vr=$?
        if [ "$_vr" = 0 ]; then _equiv=$((_equiv + 1))
        elif [ "$_vr" = 2 ]; then UNKNOWN_WHY="verbatim patch-id for reflog-only commit $c could not be established$([ -n "$PATCHID_VERBATIM" ] || printf " (this git has no patch-id --verbatim)")"; break
        else _unique="$_unique ${c:0:12}"; fi
      done
    fi
    if [ -n "$UNKNOWN_WHY" ]; then
      check 6_reflog_only_empty unknown "$UNKNOWN_WHY"
    elif [ -n "$_unique" ]; then
      NOT_DELETABLE="$BRANCH's reflog holds commit(s) no ref or landed patch holds:$_unique"
      check 6_reflog_only_empty fail "$NOT_DELETABLE"
    else
      check 6_reflog_only_empty pass "${#_named[@]} reflog entr(ies); $_n reflog-only commit(s), all $_equiv verbatim-equivalent to $DEFAULT_REF"
    fi
  fi
fi

if [ -n "$UNKNOWN_WHY" ]; then
  finish UNKNOWN "$UNKNOWN_WHY"
fi

if [ -z "$NOT_DELETABLE" ]; then
  race_hook
  # Check 3 again, immediately before the delete: a checkout, rebase or bisect
  # that started while checks 4-6 ran is invisible to `update-ref -d`.
  branch_busy; _bb=$?
  if [ "$_bb" != 0 ]; then
    check 3_recheck "$([ "$_bb" = 2 ] && echo unknown || echo fail)" "$BUSY_WHY"
    finish "$([ "$_bb" = 2 ] && echo UNKNOWN || echo REFUSED)" "$BUSY_WHY (re-checked immediately before the delete); nothing deleted"
  fi
  if ! gw update-ref -d "refs/heads/$BRANCH" "$SNAP_SHA" 2>"$TMP/del.err"; then
    check delete fail "compare-and-delete of refs/heads/$BRANCH refused: $(head -1 "$TMP/del.err")"
    finish REFUSED "refs/heads/$BRANCH moved between check and delete; nothing deleted"
  fi
  DELETED+=("refs/heads/$BRANCH")
  if g config --local --get-regexp "^branch\\.$(printf '%s' "$BRANCH" | sed 's/[].[^$*+?(){}|\\]/\\&/g')\\." >/dev/null 2>&1; then
    gw config --local --remove-section "branch.$BRANCH" 2>/dev/null \
      && check config pass "removed branch.$BRANCH.*" \
      || check config fail "could not remove branch.$BRANCH.* (the branch ref is already gone; the section is inert)"
  fi
  if delete_snapshot; then finish REAPED "$BRANCH landed and holds nothing reflog-only"; fi
  check delete fail "compare-and-delete of $WIP_REF refused after the branch was deleted: $(head -1 "$TMP/del.err")"
  finish REFUSED "branch deleted but the snapshot moved before its delete; the snapshot is kept"
fi

# The branch stays. Is the snapshot redundant?
HOLDER=""
if g merge-base --is-ancestor "$SNAP_SHA" "$DEFAULT_REF" 2>/dev/null; then
  HOLDER="$DEFAULT_REF"
else
  while IFS= read -r _b; do
    [ -n "$_b" ] || continue
    if g merge-base --is-ancestor "$SNAP_SHA" "refs/heads/$_b" 2>/dev/null; then HOLDER="refs/heads/$_b"; break; fi
  done < <(g for-each-ref --format='%(refname:strip=2)' refs/heads | tr -d '\r' | awk -v b="$BRANCH" '{ print ($0 == b ? 0 : 1) "\t" $0 }' | sort -k1,1 -s | cut -f2)
fi
if [ -n "$HOLDER" ]; then
  check holder pass "$HOLDER still contains $SNAP_SHA"
  race_hook
  if delete_snapshot; then finish SNAPSHOT_ONLY "branch kept ($NOT_DELETABLE); the snapshot is redundant because $HOLDER holds its commit"; fi
  check delete fail "compare-and-delete of $WIP_REF refused: $(head -1 "$TMP/del.err")"
  finish REFUSED "the snapshot moved between check and delete"
fi
check holder fail "no local branch and not $DEFAULT_REF contains $SNAP_SHA: the snapshot is its only holder"
finish REFUSED "$NOT_DELETABLE; the snapshot is the only ref holding $SNAP_SHA"
