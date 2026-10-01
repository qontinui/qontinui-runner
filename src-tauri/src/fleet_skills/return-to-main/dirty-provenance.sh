#!/bin/bash
# dirty-provenance.sh — is this checkout's dirt already on main (or only
# PROVISIONER RESIDUE)?
#
# Plan 2026-09-13-nightly-return-to-main-sweep, Phase 3b; extended by plan
# 2026-09-29 "the return-to-main sweep never clears WIP that is already on
# main", Phases 1-2. The sweep abstains on any uncommitted content
# (classify-branch-state.sh forces UNIQUE_WIP on a dirty tree), so a checkout
# holding WIP that already LANDED is pinned behind forever. This helper
# classifies every dirty path by exact blob (and mode) match, so the judgment
# step can decide whether `--restore-residue REPO` is safe. It decides nothing
# and changes nothing. `scripts/wip-on-main.sh` is the same script in
# `--summary` mode, named by the question it answers.
#
# CLASSES, per dirty tracked file. A file has one or two versions (the worktree
# blob, and the index blob when it differs from HEAD). Tested in this order:
#   (mode-only)          a version's blob EQUALS HEAD's blob at that path, so
#                        what git reports as modified is not content (a mode
#                        change, or a staged change reverted in the worktree).
#                        Classed UNIQUE: no blob arm can say anything about it.
#   (filtered bytes)     checking the STORED blob (`hash-object`: clean filters
#                        and eol conversion applied) back out at this path
#                        (`cat-file --filters`: smudge + eol conversion, which
#                        is what a restore, a fast-forward or a snapshot apply
#                        writes) does not reproduce the file's RAW bytes
#                        (`hash-object --no-filters`) -- a lossy `filter=`
#                        driver, a mixed-EOL file under autocrlf/text, or an LF
#                        file under `eol=crlf` whose checkout would be CRLF.
#                        The round trip is checked whenever the raw and stored
#                        hashes differ AND whenever any conversion is in effect
#                        for the path (`check-attr` text/eol/filter/ident/
#                        working-tree-encoding set, the legacy `crlf` attribute
#                        in any state, or core.autocrlf `input` or any boolean
#                        true -- yes/on/1 included; a malformed value fails
#                        closed), since equal hashes do not prove the
#                        checkout writes the same bytes. Every blob arm below compares
#                        the STORED form, so a match would prove nothing about
#                        the bytes a restore destroys. A pure CRLF<->LF
#                        conversion round-trips and is NOT this (a Windows
#                        core.autocrlf box stays restorable). A stored blob
#                        that is not in the object store cannot be checked out,
#                        so it fails closed. Classed UNIQUE; an untracked such
#                        file is never in untracked_on_upstream.
#   (occupied path)      worktree only: a leading component of the path is a
#                        non-directory or a symlink on disk (an ignored file
#                        `a` where git has `a/b`), or -- for a deletion -- the
#                        path itself exists (an ignored file, a directory of
#                        ignored files). A restore would have to replace or
#                        remove what is there, which no snapshot captures.
#                        Classed UNIQUE; such an untracked file is never listed
#                        in untracked_on_upstream.
#   (attributes unstable) worktree only: a `.gitattributes` is itself dirty, or
#                        one differs between HEAD and the upstream tip (or that
#                        diff cannot be read), AND the file has a conversion in
#                        effect under the current attributes or under the
#                        tip's (`check-attr --source=<upstream>`). The
#                        round-trip answer above was computed under attributes
#                        the restore / fast-forward will not use. Classed
#                        UNKNOWN (exit 3); such an untracked file is never
#                        listed. Deletions are exempt: they write no bytes the
#                        fast-forward keeps.
#   (symlink)            a version whose worktree or index entry is a symlink
#                        (120000). Classed UNIQUE: the residue reaper refuses
#                        every symlink entry, so a restored one would sit in a
#                        snapshot nothing can retire. An untracked symlink is
#                        never listed in untracked_on_upstream.
#   (mode change)        a version's mode differs from BOTH HEAD's and the tip's
#                        mode at that path: a deliberate chmod no upstream blob
#                        reproduces. Classed UNIQUE.
#   UPSTREAM_CURRENT     EVERY version equals `<upstream>:<path>` at the TIP,
#                        blob AND mode (the index entry's mode; for the worktree
#                        the executable bit, when core.fileMode is on). Decided
#                        for all paths in one batched `git ls-tree` of the tip.
#                        A pure deletion (` D` / `D `) of a path that is ALSO
#                        absent at the tip is UPSTREAM_CURRENT too; a deletion
#                        of a path the tip still has is UNIQUE, and so is a
#                        deletion whose path is OCCUPIED on disk (an ignored
#                        file, or a directory holding only ignored files): a
#                        restore would overwrite or remove it. Only when an
#                        upstream resolves. Tip content under a different mode
#                        is NOT current (the fast-forward would not reproduce
#                        the local mode) and falls through to the arms below --
#                        UNIQUE by "(mode change)" when the mode matches
#                        neither HEAD's nor the tip's.
#   UPSTREAM_HISTORICAL  the blob occurs at that path somewhere in the upstream
#                        default branch's history. ONE batched `git log <up>
#                        --raw` over the still-undecided paths builds the
#                        blob/path set (the same match `--find-object` makes).
#                        A provisioner writing an older bundle generation, or
#                        WIP that landed and was later superseded, lands here.
#   RUNNER_BUNDLE        .claude/commands|skills only: the blob equals the RUNNING
#                        runner build's bundled copy (GET /health gitSha, read
#                        through scripts/lib/pinned-read.sh against the sibling
#                        qontinui-runner checkout — the same lookup as
#                        .claude/hooks/provisioner-overwrite-advisory.sh), OR it
#                        occurs in the bundle path's history on runner's upstream.
#   EOL_ONLY             differs from HEAD only by carriage returns.
#   UNIQUE               none of the above — content nothing upstream has.
#   UNKNOWN              a probe that could have made it residue could not run:
#                        no upstream ref, a failed history read (reason "history
#                        read on <up> failed"), a failed tip read for a
#                        deletion, or a bundle member whose running build could
#                        not be read (runner down, sha not in the local runner
#                        clone). Never folded into UNIQUE or into residue
#                        [policy: verification-and-evidence
#                        `silent-empty-is-unknown`].
# A file is UPSTREAM_CURRENT only when every version is tip-equal; otherwise it
# is UNIQUE if any version is. Type changes, staged renames, unmerged entries
# and a deletion beside a content change (`AD`, `MD`) are UNIQUE.
# UNTRACKED files (worktree `??` records; in --stash mode the `^3` untracked
# tree) are never classified into files[] and never affect the verdict or the
# exit code. One whose bytes AND mode equal `<upstream>:<path>` at the tip is
# listed in `untracked_on_upstream`; every other one is only counted.
#
# RESTORABLE — what a residue restore may act on (per file, `restorable` plus
# `restorable_reason`). A class is a statement about BYTES:
#   UPSTREAM_CURRENT     restorable ANYWHERE. Restoring the file and then
#                        fast-forwarding leaves the identical bytes (and mode)
#                        on disk, or, for a deletion, deletes it again. Nothing,
#                        deliberate or not, is lost.
#   UPSTREAM_HISTORICAL  restorable ANYWHERE. Bytes alone cannot tell a
#                        provisioner's older write from a person's in-flight
#                        deliberate revert to an older version — but the bytes
#                        themselves are in upstream history, the sweep restores
#                        only under a QUIET quiesce verdict (no live session
#                        can be mid-edit) or the per-repo override's idle test,
#                        and it writes a refs/wip/ snapshot BEFORE restoring,
#                        so the exact prior state stays recoverable.
#   RUNNER_BUNDLE, EOL_ONLY  restorable only inside the RUNNER PROVISIONER'S
#                        FOOTPRINT — the trees the runner's fleet_commands /
#                        fleet_skills provisioners write:
#                            .claude/commands/**   .claude/skills/**
#                        Outside it they are not restorable (reason "outside
#                        provisioner footprint"): a deliberate CRLF change is
#                        byte-identical to EOL residue, and nothing else in
#                        upstream carries those bytes.
#   UNIQUE, UNKNOWN      never restorable. A mode-only change is UNIQUE (above).
#
# READ-ONLY. `--no-optional-locks` on every git call; no ref, index or file in
# the checkout is written. `--stash <rev>` evaluates a stash/snapshot commit's
# tree against its first parent (index = ^2, untracked = ^3) with no checkout.
# Neither `git log` nor `git ls-tree` takes --pathspec-from-file, so every
# per-path read is chunked into bounded argv batches (DIRTY_PROVENANCE_CHUNK_BYTES,
# default 16000), each path carrying `:(literal)` pathspec magic.
#
# USAGE
#   dirty-provenance.sh [--json | --summary] [--stash <rev>] [--upstream <ref>]
#                       [--runner-repo <dir>] <checkout>
#   --json            one JSON object on stdout:
#                     {"checkout","mode","stash","head","upstream","runner":{...},
#                      "files":[{"path","class","evidence","status",
#                                "restorable","restorable_reason"}],
#                      "counts":{"UPSTREAM_CURRENT","UPSTREAM_HISTORICAL",
#                                "RUNNER_BUNDLE","EOL_ONLY","UNIQUE","UNKNOWN"},
#                      "all_residue","unique_count","unknown_count",
#                      "outside_footprint_count","restorable_count",
#                      "untracked_count","untracked_on_upstream":[<path>...],
#                      "untracked_on_upstream_count","verdict","error"}
#                     `all_residue` is true iff EVERY dirty tracked file is
#                     restorable (vacuously true when none is), and is exactly
#                     the exit-0 condition. `untracked_count` is ALL untracked
#                     files; `untracked_on_upstream` lists the tip-equal ones
#                     as plain strings (never as files[] objects).
#   --summary         the token-lean answer to "is this WIP already on main?",
#                     one `key: value` line each (same exit codes):
#                       dirty-provenance summary: <checkout> (worktree|snapshot <rev>)
#                       upstream: <ref>|<none>
#                       <CLASS> <count>      one line per class, in counts order
#                       restorable: <n> of <m> tracked
#                       verdict: <VERDICT> exit=<rc>
#                       error: <text>        only when the run could not read
#                       not-restorable: <CLASS> <path>   one per such file
#                       untracked: <t> total, <a> on upstream, <b> not on upstream
#                       untracked-not-on-upstream: <path>  one per such file
#                     No evidence strings; `--json` has them. Mutually
#                     exclusive with --json (usage error).
#   --stash <rev>     evaluate <rev> (a `git stash create`/refs/wip snapshot)
#                     instead of the working tree.
#   --upstream <ref>  upstream default ref (default origin/HEAD, origin/main,
#                     origin/master — first that resolves).
#   --runner-repo <d> qontinui-runner checkout (default $QONTINUI_RUNNER_REPO,
#                     else the nearest ancestor holding qontinui-runner/).
# ENV  QONTINUI_RUNNER_HEALTH (default http://127.0.0.1:9876/health; any curl
#      URL, file:// included), QONTINUI_HEALTH_TIMEOUT (default 12 s),
#      DIRTY_PROVENANCE_LIB_DIR (where lib/*.sh live; default beside this file),
#      DIRTY_PROVENANCE_CHUNK_BYTES (argv budget per batched git read).
#
# EXIT  0 every dirty tracked file is restorable (vacuously true when none is)
#       1 at least one file is DECIDED not restorable: a UNIQUE file (verdict
#         UNIQUE_PRESENT), or a RUNNER_BUNDLE/EOL_ONLY file outside the
#         provisioner footprint (verdict OUTSIDE_FOOTPRINT when none is UNIQUE)
#       3 UNKNOWN/INCOMPLETE — nothing decided-unrestorable, but some file
#         could not be decided, or the checkout/snapshot could not be read
#       4 usage error
# ---- END HELP

set -u

_dp_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="${DIRTY_PROVENANCE_LIB_DIR:-$_dp_dir/lib}"

if [ -r "$LIB_DIR/git-scope.sh" ]; then . "$LIB_DIR/git-scope.sh"; fi
if declare -F git_scope_strip >/dev/null 2>&1; then
  git_scope_strip
elif [ -n "${GIT_DIR+s}${GIT_WORK_TREE+s}${GIT_COMMON_DIR+s}" ]; then
  echo "dirty-provenance: FATAL - lib/git-scope.sh is not usable ($LIB_DIR) AND GIT_DIR/GIT_WORK_TREE/GIT_COMMON_DIR is set; every answer would be about another repository. Refusing." >&2
  exit 4
fi
if [ -r "$LIB_DIR/native-path.sh" ]; then . "$LIB_DIR/native-path.sh"; fi
if ! declare -F native_path_w >/dev/null 2>&1; then
  if command -v cygpath >/dev/null 2>&1; then
    echo "dirty-provenance: FATAL - lib/native-path.sh is not usable ($LIB_DIR) on an MSYS box. Refusing." >&2
    exit 4
  fi
  native_path_w() { printf '%s\n' "$1"; }
fi
PINNED_READ="$LIB_DIR/pinned-read.sh"
[ -r "$PINNED_READ" ] || PINNED_READ=""

# Every revspec below is `<rev>:<path>`, and `.claude/...` paths are exactly the
# spelling MSYS mangles, so conversion is off for the whole run and every path
# handed to a native binary goes through native_path_w explicitly.
export MSYS_NO_PATHCONV=1

HEALTH_URL="${QONTINUI_RUNNER_HEALTH:-http://127.0.0.1:9876/health}"   # runner-localhost-ok
HEALTH_TIMEOUT="${QONTINUI_HEALTH_TIMEOUT:-12}"
case "$HEALTH_TIMEOUT" in ''|*[!0-9]*) HEALTH_TIMEOUT=12 ;; esac
CHUNK_BYTES="${DIRTY_PROVENANCE_CHUNK_BYTES:-16000}"
case "$CHUNK_BYTES" in ''|*[!0-9]*) CHUNK_BYTES=16000 ;; esac

usage() { sed -n '2,/^# ---- END HELP/p' "$0" | sed '$d' | sed 's/^#\{0,1\} \{0,1\}//'; }

MODE=human; STASH=""; UP_OVERRIDE=""; RUNNER_REPO="${QONTINUI_RUNNER_REPO:-}"; CHECKOUT=""
set_mode() {
  if [ "$MODE" != human ] && [ "$MODE" != "$1" ]; then echo "dirty-provenance: --json and --summary are mutually exclusive" >&2; exit 4; fi
  MODE="$1"
}
while [ $# -gt 0 ]; do
  case "$1" in
    --json) set_mode json; shift ;;
    --summary) set_mode summary; shift ;;
    --stash|--upstream|--runner-repo)
      [ $# -ge 2 ] && [ -n "$2" ] || { echo "dirty-provenance: $1 needs a value" >&2; exit 4; }
      case "$1" in --stash) STASH="$2" ;; --upstream) UP_OVERRIDE="$2" ;; *) RUNNER_REPO="$2" ;; esac
      shift 2 ;;
    -h|--help) usage; exit 0 ;;
    --) shift; [ $# -gt 0 ] && { CHECKOUT="$1"; shift; }; [ $# -eq 0 ] || { echo "dirty-provenance: one checkout at a time" >&2; exit 4; } ;;
    -*) echo "dirty-provenance: unknown option $1" >&2; exit 4 ;;
    *) [ -z "$CHECKOUT" ] || { echo "dirty-provenance: one checkout at a time" >&2; exit 4; }; CHECKOUT="$1"; shift ;;
  esac
done
[ -n "$CHECKOUT" ] || { echo "dirty-provenance: a <checkout> is required (see --help)" >&2; exit 4; }
[ -d "$CHECKOUT" ] || { echo "dirty-provenance: no such directory: $CHECKOUT" >&2; exit 4; }
CHECKOUT="$(cd "$CHECKOUT" && pwd)"
CO_N="$(native_path_w "$CHECKOUT")"

g()  { git --no-optional-locks -c core.quotePath=false -C "$CO_N" "$@"; }
rg() { git --no-optional-locks -c core.quotePath=false -C "$RUNNER_N" "$@"; }

TMP="$(mktemp -d)" || { echo "dirty-provenance: cannot mktemp -d" >&2; exit 3; }
trap 'rm -rf "$TMP"' EXIT

json_escape() {
  local s="$1"
  s="${s//\\/\\\\}"; s="${s//\"/\\\"}"; s="${s//$'\n'/\\n}"; s="${s//$'\r'/\\r}"; s="${s//$'\t'/\\t}"
  # Fork `tr` only for the rare string that still holds a control character:
  # emit runs this several times per file, and a fork each made --json ~6x the
  # classification cost on a 400-file tree.
  case "$s" in
    *[$'\001'-$'\010'$'\013'$'\014'$'\016'-$'\037']*) printf '%s' "$s" | tr -d '\000-\010\013\014\016-\037' ;;
    *) printf '%s' "$s" ;;
  esac
}
jstr() { if [ -n "${1:-}" ]; then printf '"'; json_escape "$1"; printf '"'; else printf 'null'; fi; }
# One line of --summary text: a path with a newline or tab stays on one line.
sline() { local s="$1"; s="${s//$'\n'/\\n}"; s="${s//$'\r'/\\r}"; s="${s//$'\t'/\\t}"; printf '%s' "$s"; }

# ---------------------------------------------------------------------------
# results
F_PATH=(); F_CLASS=(); F_EVID=(); F_STATUS=(); F_REST=(); F_RREASON=()
FILTERED_EVID="checking the stored blob back out (clean filter / eol conversion) does not reproduce the on-disk bytes: restoring would lose the raw bytes"
ATTR_EVID="attributes_unstable: a .gitattributes is dirty or differs between HEAD and the upstream tip, and this file has a conversion in effect, so whether a restore reproduces its bytes was decided under attributes the restore/fast-forward will not use"
MODE_EVID="mode change is not reproducible from upstream"
SYMLINK_EVID="symlink entry: a symlink is never restored as residue (its snapshot could never be reaped)"
UNTRACKED=0; UNT_ON_UP=(); UNT_OTHER=()
FATAL=""

# The runner provisioner's footprint (see RESTORABLE in the header).
in_footprint() { case "$1" in .claude/commands/?*|.claude/skills/?*) return 0 ;; *) return 1 ;; esac; }

add_file() { # <path> <class> <evidence> <status>; derives restorable + reason
  local r=false why
  case "$2" in
    UPSTREAM_CURRENT)
      r=true; why="UPSTREAM_CURRENT: equals the upstream tip (blob and mode), so restoring it and fast-forwarding reproduces these exact bytes (or deletes the path again) and nothing is lost" ;;
    UPSTREAM_HISTORICAL)
      r=true; why="UPSTREAM_HISTORICAL: these bytes are in upstream history at this path, and the sweep restores only under a QUIET verdict after snapshotting to refs/wip, so the prior state stays recoverable" ;;
    RUNNER_BUNDLE|EOL_ONLY)
      if in_footprint "$1"; then r=true; why="residue class inside the provisioner footprint (.claude/commands/**, .claude/skills/**)"
      else why="outside provisioner footprint: a $2 file here is byte-identical to a deliberate edit (a CRLF change, a hand-copied bundle file), and only .claude/commands/** and .claude/skills/** are the provisioner's"; fi ;;
    UNIQUE) why="class UNIQUE: content nothing upstream has, never restored" ;;
    *) why="class $2: undecided, never restored" ;;
  esac
  F_PATH+=("$1"); F_CLASS+=("$2"); F_EVID+=("$3"); F_STATUS+=("$4"); F_REST+=("$r"); F_RREASON+=("$why")
}

emit() {
  local i n_cur=0 n_hist=0 n_bun=0 n_eol=0 n_uniq=0 n_unk=0 n_out=0 n_rest=0 rc verdict all p
  for i in "${!F_CLASS[@]}"; do
    case "${F_CLASS[$i]}" in
      UPSTREAM_CURRENT) n_cur=$((n_cur+1)) ;; UPSTREAM_HISTORICAL) n_hist=$((n_hist+1)) ;;
      RUNNER_BUNDLE) n_bun=$((n_bun+1)) ;; EOL_ONLY) n_eol=$((n_eol+1)) ;;
      UNIQUE) n_uniq=$((n_uniq+1)) ;; *) n_unk=$((n_unk+1)) ;;
    esac
    if [ "${F_REST[$i]}" = true ]; then n_rest=$((n_rest+1))
    else case "${F_CLASS[$i]}" in RUNNER_BUNDLE|EOL_ONLY) n_out=$((n_out+1)) ;; esac; fi
  done
  if [ -n "$FATAL" ]; then verdict=UNKNOWN; rc=3
  elif [ "$n_uniq" -gt 0 ]; then verdict=UNIQUE_PRESENT; rc=1
  elif [ "$n_out" -gt 0 ]; then verdict=OUTSIDE_FOOTPRINT; rc=1
  elif [ "$n_unk" -gt 0 ]; then verdict=UNKNOWN; rc=3
  else verdict=ALL_RESIDUE; rc=0; fi
  # all_residue == "every dirty tracked file is restorable" == exit 0: rc 0
  # leaves no UNIQUE, no UNKNOWN and no footprint-bound file outside the footprint.
  all=false; [ "$rc" -eq 0 ] && all=true
  if [ "$MODE" = json ]; then
    printf '{"checkout":%s,"mode":%s,"stash":%s,"head":%s,"upstream":%s,' \
      "$(jstr "$CHECKOUT")" "$(jstr "$([ -n "$STASH" ] && echo stash || echo worktree)")" \
      "$(jstr "$STASH")" "$(jstr "${BASE_SHA:-}")" "$(jstr "${UPSTREAM:-}")"
    printf '"runner":{"repo":%s,"upstream":%s,"build_sha":%s,"build_arm":%s,"build_reason":%s},' \
      "$(jstr "${RUNNER_REPO:-}")" "$(jstr "${RUNNER_UP:-}")" "$(jstr "${BUILD_SHA:-}")" \
      "$(jstr "${BUILD_STATE:-not_needed}")" "$(jstr "${BUILD_REASON:-}")"
    printf '"files":['
    for i in "${!F_PATH[@]}"; do   # jstr straight to stdout: no subshell per field
      [ "$i" -gt 0 ] && printf ','
      printf '{"path":'; jstr "${F_PATH[$i]}"; printf ',"class":'; jstr "${F_CLASS[$i]}"
      printf ',"evidence":'; jstr "${F_EVID[$i]}"; printf ',"status":'; jstr "${F_STATUS[$i]}"
      printf ',"restorable":%s,"restorable_reason":' "${F_REST[$i]}"; jstr "${F_RREASON[$i]}"; printf '}'
    done
    printf '],"counts":{"UPSTREAM_CURRENT":%d,"UPSTREAM_HISTORICAL":%d,"RUNNER_BUNDLE":%d,"EOL_ONLY":%d,"UNIQUE":%d,"UNKNOWN":%d},' \
      "$n_cur" "$n_hist" "$n_bun" "$n_eol" "$n_uniq" "$n_unk"
    printf '"all_residue":%s,"unique_count":%d,"unknown_count":%d,"outside_footprint_count":%d,"restorable_count":%d,"untracked_count":%d,' \
      "$all" "$n_uniq" "$n_unk" "$n_out" "$n_rest" "$UNTRACKED"
    printf '"untracked_on_upstream":['
    i=0
    for p in ${UNT_ON_UP[@]+"${UNT_ON_UP[@]}"}; do [ "$i" -gt 0 ] && printf ','; jstr "$p"; i=$((i+1)); done
    printf '],"untracked_on_upstream_count":%d,"verdict":%s,"error":%s}\n' "${#UNT_ON_UP[@]}" "$(jstr "$verdict")" "$(jstr "$FATAL")"
  elif [ "$MODE" = summary ]; then
    printf 'dirty-provenance summary: %s (%s)\n' "$(sline "$CHECKOUT")" "$([ -n "$STASH" ] && printf 'snapshot %s' "$(sline "$STASH")" || printf worktree)"
    printf 'upstream: %s\n' "${UPSTREAM:-<none>}"
    printf 'UPSTREAM_CURRENT %d\nUPSTREAM_HISTORICAL %d\nRUNNER_BUNDLE %d\nEOL_ONLY %d\nUNIQUE %d\nUNKNOWN %d\n' \
      "$n_cur" "$n_hist" "$n_bun" "$n_eol" "$n_uniq" "$n_unk"
    printf 'restorable: %d of %d tracked\nverdict: %s exit=%d\n' "$n_rest" "${#F_PATH[@]}" "$verdict" "$rc"
    [ -n "$FATAL" ] && printf 'error: %s\n' "$(sline "$FATAL")"
    for i in "${!F_PATH[@]}"; do
      [ "${F_REST[$i]}" = true ] || { printf 'not-restorable: %s ' "${F_CLASS[$i]}"; sline "${F_PATH[$i]}"; printf '\n'; }
    done
    printf 'untracked: %d total, %d on upstream, %d not on upstream\n' "$UNTRACKED" "${#UNT_ON_UP[@]}" "$((UNTRACKED - ${#UNT_ON_UP[@]}))"
    for p in ${UNT_OTHER[@]+"${UNT_OTHER[@]}"}; do printf 'untracked-not-on-upstream: '; sline "$p"; printf '\n'; done
  else
    printf 'dirty-provenance: %s%s\n' "$CHECKOUT" "$([ -n "$STASH" ] && printf ' (snapshot %s)' "$STASH")"
    printf '  upstream=%s  runner build arm=%s%s\n' "${UPSTREAM:-<none>}" "${BUILD_STATE:-not_needed}" \
      "$([ -n "${BUILD_REASON:-}" ] && printf ' (%s)' "$BUILD_REASON")"
    [ -n "$FATAL" ] && printf '  ERROR: %s\n' "$FATAL"
    for i in "${!F_PATH[@]}"; do
      printf '  %-19s %s\n      %s\n      restorable=%s: %s\n' "${F_CLASS[$i]}" "${F_PATH[$i]}" "${F_EVID[$i]}" \
        "${F_REST[$i]}" "${F_RREASON[$i]}"
    done
    for p in ${UNT_ON_UP[@]+"${UNT_ON_UP[@]}"}; do printf '  %-19s %s (untracked)\n' UPSTREAM_CURRENT "$p"; done
    printf '\n  current=%d historical=%d bundle=%d eol_only=%d unique=%d unknown=%d outside_footprint=%d restorable=%d untracked=%d (on upstream %d)\n  VERDICT: %s\n' \
      "$n_cur" "$n_hist" "$n_bun" "$n_eol" "$n_uniq" "$n_unk" "$n_out" "$n_rest" "$UNTRACKED" "${#UNT_ON_UP[@]}" "$verdict"
  fi
  exit "$rc"
}

resolve_upstream() { # <git-fn> [override] -> prints a ref name
  local fn="$1" ov="${2:-}" r
  if [ -n "$ov" ]; then
    "$fn" rev-parse -q --verify "$ov^{commit}" >/dev/null 2>&1 && { printf '%s' "$ov"; return 0; }
    return 1
  fi
  r="$("$fn" symbolic-ref -q refs/remotes/origin/HEAD 2>/dev/null)"
  if [ -n "$r" ] && "$fn" rev-parse -q --verify "$r^{commit}" >/dev/null 2>&1; then printf '%s' "${r#refs/remotes/}"; return 0; fi
  for r in origin/main origin/master; do
    "$fn" rev-parse -q --verify "refs/remotes/$r^{commit}" >/dev/null 2>&1 && { printf '%s' "$r"; return 0; }
  done
  return 1
}

g rev-parse --git-dir >/dev/null 2>&1 || { echo "dirty-provenance: not a git checkout: $CHECKOUT" >&2; exit 4; }
if ! UPSTREAM="$(resolve_upstream g "$UP_OVERRIDE")"; then
  UPSTREAM=""
  [ -n "$UP_OVERRIDE" ] && { FATAL="--upstream '$UP_OVERRIDE' does not resolve to a commit"; emit; }
fi

# ---------------------------------------------------------------------------
# The runner bundle arm — resolved lazily, once, on the first bundle member.
BUILD_STATE=""; BUILD_SHA=""; BUILD_REASON=""; RUNNER_UP=""; RUNNER_N=""
bundle_path_for() {
  case "$1" in
    .claude/commands/*) printf 'src-tauri/src/fleet_commands/%s' "${1#.claude/commands/}" ;;
    .claude/skills/*)   printf 'src-tauri/src/fleet_skills/%s'   "${1#.claude/skills/}" ;;
    *) return 1 ;;
  esac
}
resolve_bundle_arm() {
  [ -z "$BUILD_STATE" ] || return 0
  BUILD_STATE=unchecked
  if [ -z "$RUNNER_REPO" ]; then
    local anc="$CHECKOUT"
    while [ -n "$anc" ] && [ "$anc" != "/" ]; do
      if [ -d "$anc/qontinui-runner/src-tauri/src/fleet_commands" ]; then RUNNER_REPO="$anc/qontinui-runner"; break; fi
      anc="$(dirname "$anc")"
    done
  fi
  if [ -z "$RUNNER_REPO" ] || [ ! -d "$RUNNER_REPO" ]; then
    RUNNER_REPO=""; BUILD_REASON="no qontinui-runner checkout found near $CHECKOUT"; return 0
  fi
  RUNNER_N="$(native_path_w "$RUNNER_REPO")"
  rg rev-parse --git-dir >/dev/null 2>&1 || { BUILD_REASON="$RUNNER_REPO is not a git checkout"; RUNNER_N=""; return 0; }
  RUNNER_UP="$(resolve_upstream rg)" || RUNNER_UP=""
  local body
  body="$(curl -fsS --max-time "$HEALTH_TIMEOUT" "$HEALTH_URL" 2>/dev/null)" || {
    BUILD_REASON="runner $HEALTH_URL did not answer (not running?) - the running build's bundle is unreadable"; return 0; }
  BUILD_SHA="$(printf '%s' "$body" | grep -o '"gitSha"[[:space:]]*:[[:space:]]*"[0-9a-f]*"' | grep -o '[0-9a-f]\{7,\}' | head -1)"
  [ -n "$BUILD_SHA" ] || BUILD_SHA="$(printf '%s' "$body" | grep -o '"buildId"[[:space:]]*:[[:space:]]*"[0-9a-f]*' | grep -o '[0-9a-f]\{7,\}' | head -1)"
  if [ -z "$BUILD_SHA" ]; then BUILD_REASON="runner /health carried no gitSha/buildId"; return 0; fi
  if ! rg cat-file -e "${BUILD_SHA}^{commit}" 2>/dev/null; then
    BUILD_REASON="running build $BUILD_SHA is not a commit in $RUNNER_REPO (fetch it)"; return 0
  fi
  if [ -z "$PINNED_READ" ]; then BUILD_REASON="lib/pinned-read.sh is not readable in $LIB_DIR"; return 0; fi
  BUILD_STATE=ok
}

hash_nocr() { tr -d '\r' | cksum; }

# ---------------------------------------------------------------------------
# Batched reads. Every per-path lookup below is a bash associative array filled
# by a handful of git calls, so the cost is O(chunks), not O(files).
declare -A TIP_M=() TIP_B=() TIP_READ=() HEAD_M=() HEAD_B=() IXE_M=() IXE_B=() WT_M=() WT_B=()
declare -A HIST=() HIST_FAILED=() CUR=() WT_FILTERED=() CONV=() TIPCONV=()
CONV_ALL=0; TIPCONV_ALL=0

# read_conv <cur|tip> <path>... -> CONV / TIPCONV [path]=1 for every path with a
# conversion in effect (text, eol, filter, ident or working-tree-encoding set to
# anything but unspecified/unset, or the legacy `crlf` attribute in ANY state
# but unspecified -- `-crlf` included, which is conservative). `tip` reads the
# upstream tip's attributes (`check-attr --source`). A failed read marks EVERY
# path (fail closed).
read_conv() {
  local tag="$1" p a v
  shift
  [ $# -gt 0 ] || return 0
  if [ "$tag" = tip ]; then
    printf '%s\0' "$@" | g check-attr -z --stdin --source="$UPSTREAM" text eol crlf filter ident working-tree-encoding > "$TMP/ca" 2>/dev/null \
      || { TIPCONV_ALL=1; return 0; }
  else
    printf '%s\0' "$@" | g check-attr -z --stdin text eol crlf filter ident working-tree-encoding > "$TMP/ca" 2>/dev/null \
      || { CONV_ALL=1; return 0; }
  fi
  while IFS= read -r -d '' p && IFS= read -r -d '' a && IFS= read -r -d '' v; do
    case "$a:$v" in *:unspecified) continue ;; crlf:*) ;; *:unset) continue ;; esac
    if [ "$tag" = tip ]; then TIPCONV["$p"]=1; else CONV["$p"]=1; fi
  done < "$TMP/ca"
}
has_conv() { [ "$CONV_ALL" = 1 ] || [ -n "${CONV[$1]+s}" ]; }
# autocrlf_converts -> 0 when core.autocrlf makes every text file convert:
# `input`, or ANY boolean true git accepts (true/yes/on/1). Unset -> 1; a read
# that errors (a malformed value) -> 0, fail closed.
autocrlf_converts() {
  local raw rc b
  raw="$(g config --get core.autocrlf 2>/dev/null)"; rc=$?
  [ "$rc" = 1 ] && return 1
  [ "$rc" = 0 ] || return 0
  case "$raw" in [Ii][Nn][Pp][Uu][Tt]) return 0 ;; esac
  b="$(g config --type=bool --get core.autocrlf 2>/dev/null)" || return 0
  [ "$b" = true ]
}
has_tipconv() { [ "$TIPCONV_ALL" = 1 ] || [ -n "${TIPCONV[$1]+s}" ]; }

# parent_blocked <path> -> 0 (BLOCKED_AT set) when a leading component of <path>
# is a non-directory or a symlink on disk: git would have to replace it.
parent_blocked() {
  local rest="$1" pre=""
  BLOCKED_AT=""
  while [ "${rest#*/}" != "$rest" ]; do
    pre="${pre:+$pre/}${rest%%/*}"; rest="${rest#*/}"
    if [ -L "$CHECKOUT/$pre" ] || { [ -e "$CHECKOUT/$pre" ] && [ ! -d "$CHECKOUT/$pre" ]; }; then
      BLOCKED_AT="$pre"; return 0
    fi
  done
  return 1
}

chunked() { # <fn> <path>... : calls <fn> with `:(literal)` pathspecs, at most CHUNK_BYTES of argv each
  local fn="$1" p bytes=0 batch=()
  shift
  for p in "$@"; do
    if [ "${#batch[@]}" -gt 0 ] && [ $((bytes + ${#p} + 13)) -gt "$CHUNK_BYTES" ]; then
      "$fn" "${batch[@]}"; batch=(); bytes=0
    fi
    batch+=(":(literal)$p"); bytes=$((bytes + ${#p} + 13))
  done
  [ "${#batch[@]}" -eq 0 ] || "$fn" "${batch[@]}"
}

TREE_REV=""; TREE_TAG=""; TREE_FAILED=0
_tree_chunk() { # ls-tree <TREE_REV> for one chunk -> <TREE_TAG>_M / _B (+ TIP_READ)
  local rec meta p a
  if ! g ls-tree -r -z "$TREE_REV" -- "$@" > "$TMP/lt" 2>/dev/null; then TREE_FAILED=1; return 0; fi
  if [ "$TREE_TAG" = tip ]; then for a in "$@"; do TIP_READ["${a#:(literal)}"]=1; done; fi
  while IFS= read -r -d '' rec; do
    meta="${rec%%$'\t'*}"; p="${rec#*$'\t'}"
    case "$TREE_TAG" in
      tip)  TIP_M["$p"]="${meta%% *}"; TIP_B["$p"]="${meta##* }" ;;
      head) HEAD_M["$p"]="${meta%% *}"; HEAD_B["$p"]="${meta##* }" ;;
    esac
  done < "$TMP/lt"
}
read_tree() { # <tag> <rev> <path>...
  TREE_TAG="$1"; TREE_REV="$2"; shift 2
  [ $# -gt 0 ] || return 0
  chunked _tree_chunk "$@"
}

_index_chunk() { # ls-files -s (stage 0) for one chunk -> IXE_M / IXE_B
  local rec meta p
  g ls-files -s -z -- "$@" > "$TMP/lf" 2>/dev/null || { TREE_FAILED=1; return 0; }
  while IFS= read -r -d '' rec; do
    meta="${rec%%$'\t'*}"; p="${rec#*$'\t'}"
    case "$meta" in *' 0') IXE_M["$p"]="${meta%% *}"; meta="${meta#* }"; IXE_B["$p"]="${meta%% *}" ;; esac
  done < "$TMP/lf"
}

FILEMODE=true
wt_mode() { # <path> -> the mode `git add` would record for the worktree file
  local p="$1"
  if [ -L "$CHECKOUT/$p" ]; then printf 120000; return; fi
  if [ "$FILEMODE" = true ]; then
    if [ -x "$CHECKOUT/$p" ]; then printf 100755; else printf 100644; fi; return
  fi
  # core.fileMode=false: git cannot see the bit, so it keeps the recorded mode.
  local m="${IXE_M[$p]:-${HEAD_M[$p]:-${TIP_M[$p]:-100644}}}"
  case "$m" in 100755|100644) printf '%s' "$m" ;; *) printf 100644 ;; esac
}
# Every regular file is hashed TWICE: as git would store it (clean filters and
# eol conversion applied -- the form every blob arm compares) and as the raw
# bytes on disk (`--no-filters`). When the two differ, OR any conversion is in
# effect for the path (read_conv), the stored blob is checked back out at that
# path (smudge_reproduces); only when that does not
# reproduce the raw bytes does WT_FILTERED mark the path: restoring it would
# destroy bytes no stored blob can give back, so it is never residue (see
# "(filtered bytes)" in the header). A hash or a checkout that cannot be taken
# marks it too -- raw bytes nobody compared are not proven reproducible.
#
# smudge_reproduces <path> <stored blob> <raw hash> -> 0 when git's checkout of
# <blob> at <path> is byte-identical to the raw bytes. `cat-file --batch
# --filters` cannot be batched safely (its header carries the PRE-filter size,
# so a converted body is unframeable), so this runs per file -- only for the
# files whose two hashes differ or that have a conversion in effect. Read-only: an object the store lacks fails
# closed rather than being written.
smudge_reproduces() {
  local sm
  [ -n "$2" ] && [ -n "$3" ] || return 1
  g cat-file -e "$2" 2>/dev/null || return 1
  sm="$(set -o pipefail; g cat-file --filters --path="$1" "$2" 2>/dev/null | g hash-object --no-filters --stdin 2>/dev/null)" || return 1
  [ -n "$sm" ] && [ "$sm" = "$3" ]
}
# unsafe_for_stdin_paths <path> -> 0 when `hash-object --stdin-paths` would
# misread the path: a newline cannot be one line, and a line starting with `"`
# is C-unquoted (so `"a"` would hash the file `a`); a leading backslash is
# routed the same way to be safe, and so is any carriage return (a trailing CR
# is stripped from the line, so `a<CR>` would hash the file `a`). Such paths
# are hashed one at a time on argv.
unsafe_for_stdin_paths() { case "$1" in *$'\n'*|*$'\r'*|'"'*|'\'*) return 0 ;; esac; return 1; }
hash_worktree() { # <path>... -> WT_B / WT_M (+ WT_FILTERED) for every readable one
  local p list=() out=() raw=() i r
  for p in "$@"; do
    if [ -L "$CHECKOUT/$p" ]; then
      WT_M["$p"]=120000; WT_B["$p"]="$(printf '%s' "$(readlink -- "$CHECKOUT/$p")" | g hash-object --stdin 2>/dev/null)"
    elif [ -f "$CHECKOUT/$p" ]; then
      WT_M["$p"]="$(wt_mode "$p")"
      if unsafe_for_stdin_paths "$p"; then
        WT_B["$p"]="$(g hash-object -- "$p" 2>/dev/null)"
        r="$(g hash-object --no-filters -- "$p" 2>/dev/null)"
        if [ -z "$r" ] || [ "$r" != "${WT_B[$p]}" ] || has_conv "$p"; then
          smudge_reproduces "$p" "${WT_B[$p]}" "$r" || WT_FILTERED["$p"]=1
        fi
      else
        list+=("$p")
      fi
    fi
  done
  [ "${#list[@]}" -gt 0 ] || return 0
  if printf '%s\n' "${list[@]}" | g hash-object --stdin-paths > "$TMP/hw" 2>/dev/null; then
    mapfile -t out < "$TMP/hw"
  fi
  if printf '%s\n' "${list[@]}" | g hash-object --no-filters --stdin-paths > "$TMP/hr" 2>/dev/null; then
    mapfile -t raw < "$TMP/hr"
  fi
  if [ "${#out[@]}" -eq "${#list[@]}" ]; then
    for i in "${!list[@]}"; do WT_B["${list[$i]}"]="${out[$i]}"; done
  else   # a file vanished mid-batch: hash one at a time, the unreadable stay unset
    for p in "${list[@]}"; do WT_B["$p"]="$(g hash-object -- "$p" 2>/dev/null)"; done
  fi
  if [ "${#raw[@]}" -ne "${#list[@]}" ]; then
    raw=(); for p in "${list[@]}"; do raw+=("$(g hash-object --no-filters -- "$p" 2>/dev/null)"); done
  fi
  for i in "${!list[@]}"; do
    p="${list[$i]}"
    [ -n "${raw[$i]}" ] && [ "${raw[$i]}" = "${WT_B[$p]:-}" ] && ! has_conv "$p" && continue
    smudge_reproduces "$p" "${WT_B[$p]:-}" "${raw[$i]}" || WT_FILTERED["$p"]=1
  done
}

_hist_chunk() { # ONE `git log <up> --raw` for a chunk -> HIST["<blob>:<path>"]=<newest commit>
  local tok h="" p rest o1 o2 a
  if ! g log "$UPSTREAM" --format=%x01%h -z --raw --no-abbrev --no-renames -- "$@" > "$TMP/hl" 2>/dev/null; then
    for a in "$@"; do HIST_FAILED["${a#:(literal)}"]=1; done; return 0
  fi
  while IFS= read -r -d '' tok; do
    while [ "${tok:0:1}" = $'\n' ]; do tok="${tok:1}"; done
    case "$tok" in
      $'\001'*) h="${tok:1}" ;;
      :*) IFS= read -r -d '' p || break
          rest="${tok#* }"; rest="${rest#* }"; o1="${rest%% *}"; rest="${rest#* }"; o2="${rest%% *}"
          case "$o1" in *[!0]*) [ -n "${HIST["$o1:$p"]+s}" ] || HIST["$o1:$p"]="$h" ;; esac
          case "$o2" in *[!0]*) [ -n "${HIST["$o2:$p"]+s}" ] || HIST["$o2:$p"]="$h" ;; esac ;;
    esac
  done < "$TMP/hl"
}

# mode_unreproducible <path> <mode> -> 0 when <mode> differs from BOTH HEAD's
# and the tip's mode at <path> (at least one of them present): a deliberate
# chmod, which no upstream blob -- current or historical -- brings back.
mode_unreproducible() {
  local hm="${HEAD_M[$1]:-}" tm="${TIP_M[$1]:-}"
  [ -n "$2" ] || return 1
  [ -n "$hm$tm" ] || return 1
  [ "$2" != "$hm" ] && [ "$2" != "$tm" ]
}

# classify_version <path> <blob> <rawfile> <mode> -> sets V_CLASS, V_EVID
# (<rawfile> is the worktree file, or empty to read <blob> from the object db)
classify_version() {
  local p="$1" b="$2" raw="$3" m="${4:-}" h bp prc bd reasons="" hb
  V_CLASS=""; V_EVID=""
  # Mode-only: the content IS HEAD's, so the modification git reports is not
  # content, and every arm below would call HEAD's own blob "historical".
  hb="${HEAD_B[$p]:-}"
  if [ -n "$hb" ] && [ "$hb" = "$b" ]; then
    V_CLASS=UNIQUE; V_EVID="blob ${b:0:12} equals HEAD's: the change is not content (a mode-only change, or a staged change reverted in the worktree), which no provisioner blob proves"; return
  fi
  if mode_unreproducible "$p" "$m"; then
    V_CLASS=UNIQUE; V_EVID="$MODE_EVID (mode $m; HEAD ${HEAD_M[$p]:-<none>}, tip ${TIP_M[$p]:-<none>})"; return
  fi
  if [ -n "$UPSTREAM" ]; then
    if [ -n "${HIST_FAILED[$p]+s}" ]; then reasons="history read on $UPSTREAM failed"
    else
      h="${HIST["$b:$p"]:-}"
      if [ -n "$h" ]; then V_CLASS=UPSTREAM_HISTORICAL; V_EVID="blob ${b:0:12} occurs at this path in $UPSTREAM history (commit $h)"; return; fi
    fi
  else reasons="no upstream ref, so the history arm could not run"; fi
  if bp="$(bundle_path_for "$p")"; then
    resolve_bundle_arm
    if [ "$BUILD_STATE" = ok ]; then
      bash "$PINNED_READ" --root "$RUNNER_N" exists "$BUILD_SHA" "$bp" >/dev/null 2>&1; prc=$?
      if [ "$prc" -eq 0 ]; then
        bd="$(bash "$PINNED_READ" --root "$RUNNER_N" cat "$BUILD_SHA" "$bp" 2>/dev/null | git hash-object --stdin 2>/dev/null)"
        if [ -n "$bd" ] && [ "$bd" = "$b" ]; then
          V_CLASS=RUNNER_BUNDLE; V_EVID="equals the running runner build ${BUILD_SHA}'s bundled $bp"; return
        fi
        [ -n "$bd" ] || reasons="${reasons:+$reasons; }bundle read at $BUILD_SHA failed"
      elif [ "$prc" -ne 1 ]; then
        reasons="${reasons:+$reasons; }pinned read of $bp at $BUILD_SHA was UNKNOWN"
      fi
    else
      reasons="${reasons:+$reasons; }${BUILD_REASON:-running build unreadable}"
    fi
    if [ -n "$RUNNER_UP" ]; then
      if h="$(rg log "$RUNNER_UP" -1 --format=%h --find-object="$b" -- ":(literal)$bp" 2>/dev/null)"; then
        if [ -n "$h" ]; then V_CLASS=RUNNER_BUNDLE; V_EVID="blob ${b:0:12} occurs at $bp in runner $RUNNER_UP history (commit $h)"; return; fi
      else reasons="${reasons:+$reasons; }runner history read failed"; fi
    elif [ -n "$RUNNER_N" ]; then
      reasons="${reasons:+$reasons; }runner checkout has no upstream ref"
    fi
  fi
  if [ -n "$hb" ]; then
    # Both sides read with their status observed: `cksum` of an EMPTY stream
    # is a valid-looking checksum, so two failed reads would otherwise agree
    # (check #73). A read that failed leaves its side empty, and empty never
    # matches.
    local mine theirs
    if [ -n "$raw" ] && [ -f "$raw" ]; then mine="$(hash_nocr < "$raw")" || mine=""
    else mine="$(set -o pipefail; g cat-file blob "$b" 2>/dev/null | hash_nocr)" || mine=""; fi
    theirs="$(set -o pipefail; g cat-file blob "$hb" 2>/dev/null | hash_nocr)" || theirs=""
    if [ -n "$mine" ] && [ -n "$theirs" ] && [ "$mine" = "$theirs" ]; then
      V_CLASS=EOL_ONLY; V_EVID="differs from ${BASE_SHA:0:12} only by carriage returns"; return
    fi
  fi
  if [ -n "$reasons" ]; then V_CLASS=UNKNOWN; V_EVID="not residue by any arm that ran; undecided because: $reasons"
  else V_CLASS=UNIQUE; V_EVID="blob ${b:0:12} is in no upstream history${bp:+, no runner bundle,} and is not an EOL-only change"; fi
}

# A version spec is "<label>=<blob>=<mode>=<rawfile>" (rawfile last: it may hold '=').
spec_parse() { local r; SP_L="${1%%=*}"; r="${1#*=}"; SP_B="${r%%=*}"; r="${r#*=}"; SP_M="${r%%=*}"; SP_R="${r#*=}"; }

# is_current <path> <spec>... -> 0 when EVERY version equals the tip (blob and mode)
is_current() {
  local p="$1" spec
  shift
  [ -n "$UPSTREAM" ] && [ -n "${TIP_READ[$p]+s}" ] && [ -n "${TIP_B[$p]:-}" ] || return 1
  for spec in "$@"; do
    spec_parse "$spec"
    [ -n "$SP_B" ] || return 1
    [ "$SP_B" != "${HEAD_B[$p]:-}" ] || return 1          # mode-only stays UNIQUE, first
    [ "$SP_B" = "${TIP_B[$p]}" ] && [ "$SP_M" = "${TIP_M[$p]}" ] || return 1
  done
  return 0
}

# classify_file <path> <status> <spec>...
classify_file() {
  local p="$1" st="$2" spec cls="" ev="" worst=""
  shift 2
  if [ -n "${CUR[$p]+s}" ]; then
    for spec in "$@"; do spec_parse "$spec"; ev="${ev:+$ev; }$SP_L: blob ${SP_B:0:12} mode $SP_M"; done
    add_file "$p" UPSTREAM_CURRENT "$ev equal $UPSTREAM's tip" "$st"; return
  fi
  for spec in "$@"; do
    spec_parse "$spec"
    if [ -z "$SP_B" ]; then V_CLASS=UNKNOWN; V_EVID="could not hash the $SP_L version"
    else classify_version "$p" "$SP_B" "$SP_R" "$SP_M"; fi
    ev="${ev:+$ev; }$SP_L: $V_EVID"
    case "$V_CLASS" in
      UNIQUE) worst=UNIQUE ;;
      UNKNOWN) [ "$worst" = UNIQUE ] || worst=UNKNOWN ;;
      *) [ -n "$cls" ] || cls="$V_CLASS" ;;
    esac
  done
  add_file "$p" "${worst:-$cls}" "$ev" "$st"
}

# classify_deletion <path> <status>: a pure deletion is lossless iff the tip lacks the path too
classify_deletion() {
  local p="$1" st="$2"
  # A deletion is a deletion only when NOTHING is at the path: an ignored file
  # (a staged `D ` with the file back on disk) or a directory of ignored files
  # (` D`) is invisible to status, and `git restore` would overwrite it or
  # remove the subtree. (--stash evaluates a snapshot, which has no disk.)
  if [ -z "$STASH" ] && { [ -e "$CHECKOUT/$p" ] || [ -L "$CHECKOUT/$p" ]; }; then
    add_file "$p" UNIQUE "deleted in git, but something occupies the path on disk (an ignored file or a directory): a restore would overwrite or remove it" "$st"; return
  fi
  # ...and only when nothing blocks a LEADING component either: an ignored file
  # (or symlink) `a` where git has `a/b` is unlinked by the restore that
  # recreates `a/b`, and no snapshot holds it.
  if [ -z "$STASH" ] && parent_blocked "$p"; then
    add_file "$p" UNIQUE "deleted in git, but $BLOCKED_AT is a file or symlink on disk where git has a directory: a restore would replace it" "$st"; return
  fi
  if [ -z "$UPSTREAM" ]; then add_file "$p" UNIQUE "deleted, and no upstream ref to compare the deletion with" "$st"
  elif [ -z "${TIP_READ[$p]+s}" ]; then add_file "$p" UNKNOWN "deleted; the read of $UPSTREAM's tip failed, so whether the tip lacks it too is undecided" "$st"
  elif [ -z "${TIP_B[$p]:-}" ]; then add_file "$p" UPSTREAM_CURRENT "deleted, and absent at $UPSTREAM's tip too: a restore recreates HEAD's copy and the fast-forward deletes it again" "$st"
  else add_file "$p" UNIQUE "deleted, but $UPSTREAM's tip still has this path (blob ${TIP_B[$p]:0:12})" "$st"; fi
}

# decide_untracked <path> <blob> <mode>
decide_untracked() {
  if [ "$3" = 120000 ]; then UNT_OTHER+=("$1"); return; fi
  if [ -z "$STASH" ] && [ -n "${WT_FILTERED[$1]+s}" ]; then UNT_OTHER+=("$1"); return; fi
  if [ -z "$STASH" ] && parent_blocked "$1"; then UNT_OTHER+=("$1"); return; fi
  if [ -z "$STASH" ] && [ "$ATTR_UNSTABLE" = 1 ] && { has_conv "$1" || has_tipconv "$1"; }; then UNT_OTHER+=("$1"); return; fi
  if [ -n "$UPSTREAM" ] && [ -n "$2" ] && [ -n "${TIP_B[$1]:-}" ] && [ "$2" = "${TIP_B[$1]}" ] && [ "$3" = "${TIP_M[$1]}" ]; then
    UNT_ON_UP+=("$1")
  else UNT_OTHER+=("$1"); fi
}

# ---------------------------------------------------------------------------
# Pass 1 collects the records; the batched reads run once over all of them;
# pass 2 decides each file in record order.
ATTR_UNSTABLE=0
R_P=(); R_ST=(); R_KIND=(); R_EV=(); R_SW=(); R_SI=()   # KIND: VER | DEL | UNIQUE | UNKNOWN (R_EV = its evidence); R_SW/R_SI = worktree/index spec
VER_P=(); ALL_P=(); UNT_P=(); UNT_B=(); UNT_M=()
if [ -n "$STASH" ]; then
  BASE_SHA="$(g rev-parse -q --verify "$STASH^1^{commit}" 2>/dev/null)" && \
    REV_SHA="$(g rev-parse -q --verify "$STASH^{commit}" 2>/dev/null)" || { FATAL="snapshot '$STASH' (or its first parent) is not a commit here"; emit; }
  IDX_SHA="$(g rev-parse -q --verify "$STASH^2^{commit}" 2>/dev/null)" || IDX_SHA=""
  if UNT_SHA="$(g rev-parse -q --verify "$STASH^3^{commit}" 2>/dev/null)"; then
    g ls-tree -r -z "$UNT_SHA" > "$TMP/ut" 2>/dev/null || { FATAL="ls-tree of the untracked parent $STASH^3 failed"; emit; }
    while IFS= read -r -d '' rec; do
      meta="${rec%%$'\t'*}"; UNT_P+=("${rec#*$'\t'}"); UNT_M+=("${meta%% *}"); UNT_B+=("${meta##* }")
    done < "$TMP/ut"
  fi
  g diff-tree -r --no-renames -z "$BASE_SHA" "$REV_SHA" > "$TMP/wt" 2>/dev/null || { FATAL="diff-tree $STASH^1..$STASH failed"; emit; }
  : > "$TMP/ix"
  if [ -n "$IDX_SHA" ]; then g diff-tree -r --no-renames -z "$BASE_SHA" "$IDX_SHA" > "$TMP/ix" 2>/dev/null || { FATAL="diff-tree of the index commit failed"; emit; }; fi
  declare -A WT_ST=() IX_ST=(); ORDER=()
  # raw records ":<m1> <m2> <o1> <o2> <S>\0<path>\0"
  while IFS= read -r -d '' meta && IFS= read -r -d '' p; do
    rest="${meta#* }"; WT_M["$p"]="${rest%% *}"; rest="${rest#* }"; rest="${rest#* }"; WT_B["$p"]="${rest%% *}"
    WT_ST["$p"]="${meta##* }"; ORDER+=("$p")
  done < "$TMP/wt"
  while IFS= read -r -d '' meta && IFS= read -r -d '' p; do
    rest="${meta#* }"; IXE_M["$p"]="${rest%% *}"; rest="${rest#* }"; rest="${rest#* }"; IXE_B["$p"]="${rest%% *}"
    IX_ST["$p"]="${meta##* }"; [ -n "${WT_ST[$p]+s}" ] || ORDER+=("$p")
  done < "$TMP/ix"
  for p in ${ORDER[@]+"${ORDER[@]}"}; do
    w="${WT_ST[$p]:-}"; x="${IX_ST[$p]:-}"
    R_P+=("$p"); R_ST+=("${x:- }${w:- }"); R_EV+=(""); R_SW+=(""); R_SI+=("")
    case "$w$x" in
      D|DD) R_KIND+=(DEL); ALL_P+=("$p"); continue ;;
      *D*) R_KIND+=(UNIQUE); R_EV[${#R_P[@]}-1]="deleted in one version while the other carries content, which no deletion rule covers"; continue ;;
      *T*) R_KIND+=(UNIQUE); R_EV[${#R_P[@]}-1]="type change"; continue ;;
    esac
    R_KIND+=(VER); VER_P+=("$p"); ALL_P+=("$p")
  done
else
  BASE_SHA="$(g rev-parse -q --verify "HEAD^{commit}" 2>/dev/null)" || { FATAL="HEAD does not resolve (unborn branch?)"; emit; }
  g status --porcelain=v1 -z --untracked-files=all > "$TMP/status" 2>/dev/null || { FATAL="git status failed in $CHECKOUT"; emit; }
  case "$(g config --bool core.fileMode 2>/dev/null)" in false) FILEMODE=false ;; esac
  while IFS= read -r -d '' rec; do
    xy="${rec:0:2}"; p="${rec:3}"; X="${xy:0:1}"
    case "$xy" in
      '??') UNT_P+=("$p"); continue ;;
      '!!') continue ;;
    esac
    R_P+=("$p"); R_ST+=("$xy"); R_EV+=(""); R_SW+=(""); R_SI+=("")
    case "$xy" in
      DD|AU|UD|UA|DU|AA|UU) R_KIND+=(UNIQUE); R_EV[${#R_P[@]}-1]="unmerged (conflict) entry"; continue ;;
    esac
    if [ "$X" = R ] || [ "$X" = C ]; then IFS= read -r -d '' orig; R_KIND+=(UNIQUE); R_EV[${#R_P[@]}-1]="staged rename/copy from $orig"; continue; fi
    case "$xy" in
      ' D'|'D ') R_KIND+=(DEL); ALL_P+=("$p"); continue ;;
      *D*) R_KIND+=(UNIQUE); R_EV[${#R_P[@]}-1]="deleted in one version while the other carries content, which no deletion rule covers"; continue ;;
      *T*) R_KIND+=(UNIQUE); R_EV[${#R_P[@]}-1]="type change"; continue ;;
    esac
    R_KIND+=(VER); VER_P+=("$p"); ALL_P+=("$p")
  done < "$TMP/status"
fi

# The batched reads: HEAD and the tip (ls-tree), the index (ls-files), the worktree (hash-object).
TREE_FAILED=0
read_tree head "$BASE_SHA" ${VER_P[@]+"${VER_P[@]}"}
[ "$TREE_FAILED" -eq 0 ] || { FATAL="ls-tree of HEAD ($BASE_SHA) failed"; emit; }
if [ -n "$UPSTREAM" ]; then read_tree tip "$UPSTREAM" ${ALL_P[@]+"${ALL_P[@]}"} ${UNT_P[@]+"${UNT_P[@]}"}; fi
if [ -z "$STASH" ]; then
  TREE_FAILED=0
  [ "${#VER_P[@]}" -eq 0 ] || chunked _index_chunk "${VER_P[@]}"
  [ "$TREE_FAILED" -eq 0 ] || { FATAL="ls-files of the index failed"; emit; }
  # Every VER record whose file is on disk is hashed -- staged-only ones (`M `,
  # `A `) included: a lossy filter shows only in the worktree bytes.
  WT_NEED=()
  for i in "${!R_P[@]}"; do
    [ "${R_KIND[$i]}" = VER ] || continue
    { [ -e "$CHECKOUT/${R_P[$i]}" ] || [ -L "$CHECKOUT/${R_P[$i]}" ]; } && WT_NEED+=("${R_P[$i]}")
  done
  autocrlf_converts && CONV_ALL=1
  read_conv cur ${WT_NEED[@]+"${WT_NEED[@]}"} ${UNT_P[@]+"${UNT_P[@]}"}
  # Attributes that the restore / fast-forward will not use: a dirty
  # .gitattributes, or one HEAD and the tip disagree on.
  ATTR_UNSTABLE=0
  for p in ${R_P[@]+"${R_P[@]}"} ${UNT_P[@]+"${UNT_P[@]}"}; do
    case "$p" in .gitattributes|*/.gitattributes) ATTR_UNSTABLE=1; break ;; esac
  done
  if [ "$ATTR_UNSTABLE" = 0 ] && [ -n "$UPSTREAM" ]; then
    _ad="$(g diff --name-only "$BASE_SHA" "$UPSTREAM" -- ':(glob)**/.gitattributes' 2>/dev/null)" || _ad=failed
    [ -z "$_ad" ] || ATTR_UNSTABLE=1
  fi
  if [ "$ATTR_UNSTABLE" = 1 ]; then
    if [ -n "$UPSTREAM" ]; then read_conv tip ${WT_NEED[@]+"${WT_NEED[@]}"} ${UNT_P[@]+"${UNT_P[@]}"}; else TIPCONV_ALL=1; fi
  fi
  hash_worktree ${WT_NEED[@]+"${WT_NEED[@]}"} ${UNT_P[@]+"${UNT_P[@]}"}
  for p in ${UNT_P[@]+"${UNT_P[@]}"}; do UNT_B+=("${WT_B[$p]:-}"); UNT_M+=("${WT_M[$p]:-}"); done
fi

# Version specs per VER record, then UPSTREAM_CURRENT for all of them at once.
HIST_NEED=()
for i in "${!R_P[@]}"; do
  [ "${R_KIND[$i]}" = VER ] || continue
  p="${R_P[$i]}"
  # A symlink is never residue: the reaper refuses every 120000 entry, so a
  # restored one would be snapshotted into a ref nothing could ever retire.
  if [ "${WT_M[$p]:-}" = 120000 ] || [ "${IXE_M[$p]:-}" = 120000 ]; then
    R_KIND[$i]=UNIQUE; R_EV[$i]="$SYMLINK_EVID"; continue
  fi
  if [ -n "$STASH" ]; then
    w="${WT_ST[$p]:-}"; x="${IX_ST[$p]:-}"
    [ -z "$w" ] || R_SW[$i]="worktree=${WT_B[$p]:-}=${WT_M[$p]:-}="
    if [ -n "$x" ] && { [ -z "$w" ] || [ "${IXE_B[$p]:-}" != "${WT_B[$p]:-}" ] || [ "${IXE_M[$p]:-}" != "${WT_M[$p]:-}" ]; }; then
      R_SI[$i]="index=${IXE_B[$p]:-}=${IXE_M[$p]:-}="
    fi
  else
    if parent_blocked "$p"; then
      R_KIND[$i]=UNIQUE; R_EV[$i]="$BLOCKED_AT is a file or symlink on disk where git has a directory: a restore would replace it"; continue
    fi
    if [ -n "${WT_FILTERED[$p]+s}" ]; then
      R_KIND[$i]=UNIQUE; R_EV[$i]="$FILTERED_EVID"; continue
    fi
    if [ "$ATTR_UNSTABLE" = 1 ] && { has_conv "$p" || has_tipconv "$p"; }; then
      R_KIND[$i]=UNKNOWN; R_EV[$i]="$ATTR_EVID"; continue
    fi
    [ "${R_ST[$i]:1:1}" = " " ] || R_SW[$i]="worktree=${WT_B[$p]:-}=${WT_M[$p]:-}=$CHECKOUT/$p"
    [ "${R_ST[$i]:0:1}" = " " ] || R_SI[$i]="index=${IXE_B[$p]:-}=${IXE_M[$p]:-}="
  fi
  specs=(); [ -z "${R_SW[$i]}" ] || specs+=("${R_SW[$i]}"); [ -z "${R_SI[$i]}" ] || specs+=("${R_SI[$i]}")
  if is_current "$p" "${specs[@]}"; then CUR["$p"]=1; else HIST_NEED+=("$p"); fi
done
if [ -n "$UPSTREAM" ] && [ "${#HIST_NEED[@]}" -gt 0 ]; then chunked _hist_chunk "${HIST_NEED[@]}"; fi

for i in "${!R_P[@]}"; do
  p="${R_P[$i]}"
  case "${R_KIND[$i]}" in
    UNIQUE) add_file "$p" UNIQUE "${R_EV[$i]}" "${R_ST[$i]}" ;;
    UNKNOWN) add_file "$p" UNKNOWN "${R_EV[$i]}" "${R_ST[$i]}" ;;
    DEL) classify_deletion "$p" "${R_ST[$i]}" ;;
    VER) specs=(); [ -z "${R_SW[$i]}" ] || specs+=("${R_SW[$i]}"); [ -z "${R_SI[$i]}" ] || specs+=("${R_SI[$i]}")
         classify_file "$p" "${R_ST[$i]}" "${specs[@]}" ;;
  esac
done
UNTRACKED="${#UNT_P[@]}"
for i in "${!UNT_P[@]}"; do decide_untracked "${UNT_P[$i]}" "${UNT_B[$i]}" "${UNT_M[$i]}"; done
emit
