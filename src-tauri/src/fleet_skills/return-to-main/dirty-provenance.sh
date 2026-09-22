#!/bin/bash
# dirty-provenance.sh — is this checkout's dirt only PROVISIONER RESIDUE?
#
# Plan 2026-09-13-nightly-return-to-main-sweep, Phase 3b. The sweep abstains on
# any uncommitted content (classify-branch-state.sh forces UNIQUE_WIP on a dirty
# tree), and the runner re-provisions `.claude/` files at spawn, so the checkout
# that DELIVERS the sweep is dirty most nights and is never updated. This helper
# classifies every modified tracked file by an exact blob match, so the judgment
# step can decide whether `--restore-residue REPO` is safe. It decides nothing
# and changes nothing.
#
# CLASSES, tested in this order per version of a file (worktree blob, and the
# index blob when it differs from HEAD):
#   (mode-only)          the blob EQUALS HEAD's blob at that path, so what git
#                        reports as modified is not content (a mode change, or
#                        a staged change reverted in the worktree). Classed
#                        UNIQUE: no blob arm can say anything about it.
#   UPSTREAM_HISTORICAL  the blob occurs at that path somewhere in the upstream
#                        default branch's history (`git log <up> --find-object`).
#                        A provisioner writing an older bundle generation lands
#                        here.
#   RUNNER_BUNDLE        .claude/commands|skills only: the blob equals the RUNNING
#                        runner build's bundled copy (GET /health gitSha, read
#                        through scripts/lib/pinned-read.sh against the sibling
#                        qontinui-runner checkout — the same lookup as
#                        .claude/hooks/provisioner-overwrite-advisory.sh), OR it
#                        occurs in the bundle path's history on runner's upstream.
#   EOL_ONLY             differs from HEAD only by carriage returns.
#   UNIQUE               none of the above — content nothing upstream has.
#   UNKNOWN              a probe that could have made it residue could not run:
#                        no upstream ref, a failed history read, or a bundle
#                        member whose running build could not be read (runner
#                        down, sha not in the local runner clone). Never folded
#                        into UNIQUE or into residue [policy:
#                        verification-and-evidence `silent-empty-is-unknown`].
# A file is UNIQUE if any of its versions is; deletions, type changes, staged
# renames and unmerged entries are UNIQUE (a provisioner never produces them).
# Untracked files are COUNTED, never classified — nothing restores them.
#
# RESTORABLE — what a residue restore may act on (per file, `restorable` plus
# `restorable_reason`). A class is a statement about BYTES, and bytes cannot
# tell a provisioner from a person: a deliberate local revert of a file to an
# older upstream version is UPSTREAM_HISTORICAL, and a deliberate CRLF change is
# EOL_ONLY, byte-for-byte identical to residue. So a file is restorable only
# when its class is UPSTREAM_HISTORICAL, RUNNER_BUNDLE or EOL_ONLY AND its path
# is inside the RUNNER PROVISIONER'S FOOTPRINT — the trees the runner's
# fleet_commands / fleet_skills provisioners write:
#     .claude/commands/**   .claude/skills/**
# (every residue file observed on nomad, 17 upstream-historical + 2
# runner-bundle, sits there). A residue-class file OUTSIDE the footprint is not
# restorable (reason "outside provisioner footprint") and makes the run exit 1,
# exactly like a UNIQUE file: it is decided, and it is not the provisioner's.
# A mode-only change is UNIQUE (above), so it is never restorable either.
#
# READ-ONLY. `--no-optional-locks` on every git call; no ref, index or file in
# the checkout is written. `--stash <rev>` evaluates a stash/snapshot commit's
# tree against its first parent (index = ^2, untracked = ^3) with no checkout.
#
# USAGE
#   dirty-provenance.sh [--json] [--stash <rev>] [--upstream <ref>]
#                       [--runner-repo <dir>] <checkout>
#   --json            one JSON object on stdout:
#                     {"checkout","mode","stash","head","upstream","runner":{...},
#                      "files":[{"path","class","evidence","status",
#                                "restorable","restorable_reason"}],
#                      "counts":{...},"all_residue","unique_count",
#                      "unknown_count","outside_footprint_count",
#                      "restorable_count","untracked_count","verdict"}
#                     `all_residue` is true iff EVERY modified tracked file is
#                     restorable (vacuously true when none is modified), and
#                     is exactly the exit-0 condition.
#   --stash <rev>     evaluate <rev> (a `git stash create`/refs/wip snapshot)
#                     instead of the working tree.
#   --upstream <ref>  upstream default ref (default origin/HEAD, origin/main,
#                     origin/master — first that resolves).
#   --runner-repo <d> qontinui-runner checkout (default $QONTINUI_RUNNER_REPO,
#                     else the nearest ancestor holding qontinui-runner/).
# ENV  QONTINUI_RUNNER_HEALTH (default http://127.0.0.1:9876/health; any curl
#      URL, file:// included), QONTINUI_HEALTH_TIMEOUT (default 12 s),
#      DIRTY_PROVENANCE_LIB_DIR (where lib/*.sh live; default beside this file).
#
# EXIT  0 every modified tracked file is restorable (vacuously true when none is)
#       1 at least one file is DECIDED not restorable: a UNIQUE file (verdict
#         UNIQUE_PRESENT), or a residue-class file outside the provisioner
#         footprint (verdict OUTSIDE_FOOTPRINT when no file is UNIQUE)
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

usage() { sed -n '2,/^# ---- END HELP/p' "$0" | sed '$d' | sed 's/^#\{0,1\} \{0,1\}//'; }

MODE=human; STASH=""; UP_OVERRIDE=""; RUNNER_REPO="${QONTINUI_RUNNER_REPO:-}"; CHECKOUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --json) MODE=json; shift ;;
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
  printf '%s' "$s" | tr -d '\000-\010\013\014\016-\037'
}
jstr() { if [ -n "${1:-}" ]; then printf '"%s"' "$(json_escape "$1")"; else printf 'null'; fi; }

# ---------------------------------------------------------------------------
# results
F_PATH=(); F_CLASS=(); F_EVID=(); F_STATUS=(); F_REST=(); F_RREASON=()
UNTRACKED=0
FATAL=""

# The runner provisioner's footprint (see RESTORABLE in the header).
in_footprint() { case "$1" in .claude/commands/?*|.claude/skills/?*) return 0 ;; *) return 1 ;; esac; }

add_file() { # <path> <class> <evidence> <status>; derives restorable + reason
  local r=false why
  case "$2" in
    UPSTREAM_HISTORICAL|RUNNER_BUNDLE|EOL_ONLY)
      if in_footprint "$1"; then r=true; why="residue class inside the provisioner footprint (.claude/commands/**, .claude/skills/**)"
      else why="outside provisioner footprint: a $2 file here is byte-identical to a deliberate edit (a local revert, a CRLF change), and only .claude/commands/** and .claude/skills/** are the provisioner's"; fi ;;
    UNIQUE) why="class UNIQUE: content nothing upstream has, never restored" ;;
    *) why="class $2: undecided, never restored"
       in_footprint "$1" || why="$why; also outside provisioner footprint" ;;
  esac
  F_PATH+=("$1"); F_CLASS+=("$2"); F_EVID+=("$3"); F_STATUS+=("$4"); F_REST+=("$r"); F_RREASON+=("$why")
}

emit() {
  local i n_hist=0 n_bun=0 n_eol=0 n_uniq=0 n_unk=0 n_out=0 n_rest=0 rc verdict all
  for i in "${!F_CLASS[@]}"; do
    case "${F_CLASS[$i]}" in
      UPSTREAM_HISTORICAL) n_hist=$((n_hist+1)) ;; RUNNER_BUNDLE) n_bun=$((n_bun+1)) ;;
      EOL_ONLY) n_eol=$((n_eol+1)) ;; UNIQUE) n_uniq=$((n_uniq+1)) ;; *) n_unk=$((n_unk+1)) ;;
    esac
    if [ "${F_REST[$i]}" = true ]; then n_rest=$((n_rest+1))
    else case "${F_CLASS[$i]}" in UPSTREAM_HISTORICAL|RUNNER_BUNDLE|EOL_ONLY) n_out=$((n_out+1)) ;; esac; fi
  done
  if [ -n "$FATAL" ]; then verdict=UNKNOWN; rc=3
  elif [ "$n_uniq" -gt 0 ]; then verdict=UNIQUE_PRESENT; rc=1
  elif [ "$n_out" -gt 0 ]; then verdict=OUTSIDE_FOOTPRINT; rc=1
  elif [ "$n_unk" -gt 0 ]; then verdict=UNKNOWN; rc=3
  else verdict=ALL_RESIDUE; rc=0; fi
  # all_residue == "every modified tracked file is restorable" == exit 0: rc 0
  # leaves no UNIQUE, no UNKNOWN and no residue-class file outside the footprint.
  all=false; [ "$rc" -eq 0 ] && all=true
  if [ "$MODE" = json ]; then
    printf '{"checkout":%s,"mode":%s,"stash":%s,"head":%s,"upstream":%s,' \
      "$(jstr "$CHECKOUT")" "$(jstr "$([ -n "$STASH" ] && echo stash || echo worktree)")" \
      "$(jstr "$STASH")" "$(jstr "${BASE_SHA:-}")" "$(jstr "${UPSTREAM:-}")"
    printf '"runner":{"repo":%s,"upstream":%s,"build_sha":%s,"build_arm":%s,"build_reason":%s},' \
      "$(jstr "${RUNNER_REPO:-}")" "$(jstr "${RUNNER_UP:-}")" "$(jstr "${BUILD_SHA:-}")" \
      "$(jstr "${BUILD_STATE:-not_needed}")" "$(jstr "${BUILD_REASON:-}")"
    printf '"files":['
    for i in "${!F_PATH[@]}"; do
      [ "$i" -gt 0 ] && printf ','
      printf '{"path":%s,"class":%s,"evidence":%s,"status":%s,"restorable":%s,"restorable_reason":%s}' "$(jstr "${F_PATH[$i]}")" \
        "$(jstr "${F_CLASS[$i]}")" "$(jstr "${F_EVID[$i]}")" "$(jstr "${F_STATUS[$i]}")" \
        "${F_REST[$i]}" "$(jstr "${F_RREASON[$i]}")"
    done
    printf '],"counts":{"UPSTREAM_HISTORICAL":%d,"RUNNER_BUNDLE":%d,"EOL_ONLY":%d,"UNIQUE":%d,"UNKNOWN":%d},' \
      "$n_hist" "$n_bun" "$n_eol" "$n_uniq" "$n_unk"
    printf '"all_residue":%s,"unique_count":%d,"unknown_count":%d,"outside_footprint_count":%d,"restorable_count":%d,"untracked_count":%d,"verdict":%s,"error":%s}\n' \
      "$all" "$n_uniq" "$n_unk" "$n_out" "$n_rest" "$UNTRACKED" "$(jstr "$verdict")" "$(jstr "$FATAL")"
  else
    printf 'dirty-provenance: %s%s\n' "$CHECKOUT" "$([ -n "$STASH" ] && printf ' (snapshot %s)' "$STASH")"
    printf '  upstream=%s  runner build arm=%s%s\n' "${UPSTREAM:-<none>}" "${BUILD_STATE:-not_needed}" \
      "$([ -n "${BUILD_REASON:-}" ] && printf ' (%s)' "$BUILD_REASON")"
    [ -n "$FATAL" ] && printf '  ERROR: %s\n' "$FATAL"
    for i in "${!F_PATH[@]}"; do
      printf '  %-19s %s\n      %s\n      restorable=%s: %s\n' "${F_CLASS[$i]}" "${F_PATH[$i]}" "${F_EVID[$i]}" \
        "${F_REST[$i]}" "${F_RREASON[$i]}"
    done
    printf '\n  historical=%d bundle=%d eol_only=%d unique=%d unknown=%d outside_footprint=%d restorable=%d untracked=%d\n  VERDICT: %s\n' \
      "$n_hist" "$n_bun" "$n_eol" "$n_uniq" "$n_unk" "$n_out" "$n_rest" "$UNTRACKED" "$verdict"
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

# classify_version <path> <blob> <rawfile> -> sets V_CLASS, V_EVID
classify_version() {
  local p="$1" b="$2" raw="$3" h bp prc bd reasons="" hb
  V_CLASS=""; V_EVID=""
  # Mode-only: the content IS HEAD's, so the modification git reports is not
  # content, and every arm below would call HEAD's own blob "historical".
  hb="$(g rev-parse -q --verify "$BASE_SHA:$p" 2>/dev/null)"
  if [ -n "$hb" ] && [ "$hb" = "$b" ]; then
    V_CLASS=UNIQUE; V_EVID="blob ${b:0:12} equals HEAD's: the change is not content (a mode-only change, or a staged change reverted in the worktree), which no provisioner blob proves"; return
  fi
  if [ -n "$UPSTREAM" ]; then
    if h="$(g log "$UPSTREAM" -1 --format=%h --find-object="$b" -- ":(literal)$p" 2>/dev/null)"; then
      if [ -n "$h" ]; then V_CLASS=UPSTREAM_HISTORICAL; V_EVID="blob ${b:0:12} occurs at this path in $UPSTREAM history (commit $h)"; return; fi
    else reasons="history read on $UPSTREAM failed"; fi
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
  if [ -n "$raw" ] && g cat-file -e "$BASE_SHA:$p" 2>/dev/null; then
    if [ "$(hash_nocr < "$raw")" = "$(g cat-file blob "$BASE_SHA:$p" 2>/dev/null | hash_nocr)" ]; then
      V_CLASS=EOL_ONLY; V_EVID="differs from ${BASE_SHA:0:12} only by carriage returns"; return
    fi
  fi
  if [ -n "$reasons" ]; then V_CLASS=UNKNOWN; V_EVID="not residue by any arm that ran; undecided because: $reasons"
  else V_CLASS=UNIQUE; V_EVID="blob ${b:0:12} is in no upstream history${bp:+, no runner bundle,} and is not an EOL-only change"; fi
}

# classify_file <path> <status> <version-spec>... ; spec = "<label>=<blob>=<rawfile>"
classify_file() {
  local p="$1" st="$2" spec lbl rest b raw cls="" ev="" worst=""
  shift 2
  for spec in "$@"; do
    lbl="${spec%%=*}"; rest="${spec#*=}"; b="${rest%%=*}"; raw="${rest#*=}"
    if [ -z "$b" ]; then V_CLASS=UNKNOWN; V_EVID="could not hash the $lbl version"
    else classify_version "$p" "$b" "$raw"; fi
    ev="${ev:+$ev; }$lbl: $V_EVID"
    case "$V_CLASS" in
      UNIQUE) worst=UNIQUE ;;
      UNKNOWN) [ "$worst" = UNIQUE ] || worst=UNKNOWN ;;
      *) [ -n "$cls" ] || cls="$V_CLASS" ;;
    esac
  done
  add_file "$p" "${worst:-$cls}" "$ev" "$st"
}

# ---------------------------------------------------------------------------
if [ -n "$STASH" ]; then
  BASE_SHA="$(g rev-parse -q --verify "$STASH^1^{commit}" 2>/dev/null)" && \
    REV_SHA="$(g rev-parse -q --verify "$STASH^{commit}" 2>/dev/null)" || { FATAL="snapshot '$STASH' (or its first parent) is not a commit here"; emit; }
  IDX_SHA="$(g rev-parse -q --verify "$STASH^2^{commit}" 2>/dev/null)" || IDX_SHA=""
  if UNT_SHA="$(g rev-parse -q --verify "$STASH^3^{commit}" 2>/dev/null)"; then
    UNTRACKED="$(g ls-tree -r --name-only "$UNT_SHA" 2>/dev/null | wc -l | tr -d ' ')"
  fi
  g diff-tree -r --no-renames -z --name-status "$BASE_SHA" "$REV_SHA" > "$TMP/wt" 2>/dev/null || { FATAL="diff-tree $STASH^1..$STASH failed"; emit; }
  : > "$TMP/ix"
  if [ -n "$IDX_SHA" ]; then g diff-tree -r --no-renames -z --name-status "$BASE_SHA" "$IDX_SHA" > "$TMP/ix" 2>/dev/null || { FATAL="diff-tree of the index commit failed"; emit; }; fi
  declare -A WT_ST=() IX_ST=(); ORDER=()
  while IFS= read -r -d '' st && IFS= read -r -d '' p; do WT_ST["$p"]="$st"; ORDER+=("$p"); done < "$TMP/wt"
  while IFS= read -r -d '' st && IFS= read -r -d '' p; do
    IX_ST["$p"]="$st"; [ -n "${WT_ST[$p]+s}" ] || ORDER+=("$p")
  done < "$TMP/ix"
  n=0
  for p in ${ORDER[@]+"${ORDER[@]}"}; do
    n=$((n+1)); w="${WT_ST[$p]:-}"; x="${IX_ST[$p]:-}"
    case "$w$x" in *D*) add_file "$p" UNIQUE "deleted in the snapshot; a provisioner never deletes" "${x:- }${w:- }"; continue ;; *T*) add_file "$p" UNIQUE "type change" "${x:- }${w:- }"; continue ;; esac
    specs=()
    if [ -n "$w" ]; then g cat-file blob "$REV_SHA:$p" > "$TMP/r$n.w" 2>/dev/null; specs+=("worktree=$(g rev-parse -q --verify "$REV_SHA:$p" 2>/dev/null)=$TMP/r$n.w"); fi
    if [ -n "$x" ]; then
      xb="$(g rev-parse -q --verify "$IDX_SHA:$p" 2>/dev/null)"
      if [ -z "$w" ] || [ "$xb" != "$(g rev-parse -q --verify "$REV_SHA:$p" 2>/dev/null)" ]; then
        g cat-file blob "$IDX_SHA:$p" > "$TMP/r$n.i" 2>/dev/null; specs+=("index=$xb=$TMP/r$n.i")
      fi
    fi
    classify_file "$p" "${x:- }${w:- }" "${specs[@]}"
  done
else
  BASE_SHA="$(g rev-parse -q --verify "HEAD^{commit}" 2>/dev/null)" || { FATAL="HEAD does not resolve (unborn branch?)"; emit; }
  g status --porcelain=v1 -z --untracked-files=all > "$TMP/status" 2>/dev/null || { FATAL="git status failed in $CHECKOUT"; emit; }
  n=0
  while IFS= read -r -d '' rec; do
    xy="${rec:0:2}"; p="${rec:3}"; X="${xy:0:1}"; Y="${xy:1:1}"; n=$((n+1))
    case "$xy" in
      '??') UNTRACKED=$((UNTRACKED+1)); continue ;;
      '!!') continue ;;
      DD|AU|UD|UA|DU|AA|UU) add_file "$p" UNIQUE "unmerged (conflict) entry" "$xy"; continue ;;
    esac
    if [ "$X" = R ] || [ "$X" = C ]; then IFS= read -r -d '' orig; add_file "$p" UNIQUE "staged rename/copy from $orig" "$xy"; continue; fi
    case "$xy" in *D*) add_file "$p" UNIQUE "deleted; a provisioner never deletes" "$xy"; continue ;; *T*) add_file "$p" UNIQUE "type change" "$xy"; continue ;; esac
    specs=()
    if [ "$Y" != " " ]; then
      if [ -f "$CHECKOUT/$p" ] && cp "$CHECKOUT/$p" "$TMP/r$n.w" 2>/dev/null; then
        specs+=("worktree=$(g hash-object -- "$p" 2>/dev/null)=$TMP/r$n.w")
      else specs+=("worktree==") ; fi
    fi
    if [ "$X" != " " ]; then
      xb="$(g rev-parse -q --verify ":$p" 2>/dev/null)"
      g cat-file blob ":$p" > "$TMP/r$n.i" 2>/dev/null
      specs+=("index=$xb=$TMP/r$n.i")
    fi
    classify_file "$p" "$xy" "${specs[@]}"
  done < "$TMP/status"
fi
emit
