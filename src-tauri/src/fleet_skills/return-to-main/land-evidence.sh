#!/bin/bash
# land-evidence.sh — did this branch's "unlanded" work actually LAND?
#
# Plan 2026-09-13-nightly-return-to-main-sweep, Phase 3a. Finding a911a385:
# qontinui-dev-notes sat on `followup/pr1260-landing-state-update` (eed39fc2)
# and the classifier said UNIQUE_WIP forever. The commit HAD landed — PR #511,
# merged as c7df1d44 after a rebase WITH conflict resolution (patch-id changed)
# and later upstream edits to the same lines (content arm failed too). Deciding
# it took a land signal plus a content comparison. This helper gathers both; the
# judgment step reads it and decides whether to pass `--adjudicated-landed`.
# It never acts and never writes to the checkout.
#
# FOR EACH unlanded commit (the classifier's `unlanded_commits`; with --ref, the
# classifier's own primitive `git cherry <upstream> <ref>`), landing CANDIDATES:
#   github_merged_pr    `gh pr list --head <branch> --state all`: a MERGED PR's
#                       merge commit. Catches real merges like #511.
#   same_subject_on_main  a commit on the upstream with the identical subject,
#                       committed no earlier than a day before the local
#                       commit's author date. Catches rebase and ff lands.
#   coord_land_stamp    coord's `merge_commit_sha` for the branch's PR, read from
#                       GET /pr-merge/prs?include_merged=<hours> on a device JWT
#                       — the door that sees fast-forward lands GitHub reports
#                       as CLOSED, mergedAt null. Asked only when the other
#                       doors did not already prove the land and a closed-
#                       unmerged PR (or a failed GitHub read) makes it relevant.
#                       (`/pr-merge/repo/<r>/prs` lists open/draft rows only, and
#                       `coord_explain_pr_close` is not on the agent principal's
#                       allow-set — measured 2026-09-13 — so neither can serve.)
# A PR-keyed candidate (github_merged_pr, coord_land_stamp) must pass two gates
# before it is a candidate for anything:
#   REACHABILITY  its merge commit is an ancestor of the upstream
#                 (`git merge-base --is-ancestor`). A PR merged into another
#                 branch — a stacked PR whose base was its parent's branch — is
#                 not a land; a merge commit absent from the local object store
#                 is not a land signal (the ref may need a fetch) and is recorded
#                 as a failed door, never as proof.
#   PAIRING       it attaches to a local commit only if that commit BELONGS to
#                 the PR: its sha is among the PR's commit oids, or its subject
#                 equals one of their subjects (`gh pr view <n> --json commits`;
#                 a truncated GitHub headline is rebuilt from the body, and the
#                 local subject is used when the oid is in the object store).
#                 Without this a merged PR's merge commit was a candidate for
#                 EVERY unlanded commit, including ones written after the merge.
#                 When pairing cannot be read, the PR is a candidate for nothing.
# same_subject_on_main is paired per commit by construction.
# For every candidate: `git range-diff` of the two commits, and the MULTISET
# presence test per file F the local commit C touches (first-parent diff),
# comparing the candidate's F against C's own post-image C:F:
#   every line L that C ADDS:    count(candidate F, L) >= count(C:F, L)
#   every line R that C REMOVES: count(candidate F, R) <= count(C:F, R)
# Counting (not set membership) is what stops a duplicate or a pre-existing line
# (a blank line, a brace) from standing in for one C added, and the removal arm
# is what catches a deletion that never landed. A file C deletes must be absent
# from, or empty in, the candidate. A GITLINK (a submodule pointer: mode 160000
# on either side of C) and a path whose diff has no lines (a mode change, an
# empty file added) are compared as TREE ENTRIES — mode plus object id — between
# C's post-image and the candidate. A gitlink is never line-counted: its object
# is not a blob here, so both sides would read as empty and any pointer would
# pass. A mode C sets on a path it keeps (an add included) must be the
# candidate's mode as well, and a binary change must match blob-for-blob.
# The candidate side is the land commit's own tree, never the upstream tip, so
# upstream edits made after the land do not enter the comparison; and the two
# inequalities point the safe way — a candidate holding FEWER copies of an added
# line or MORE copies of a removed one fails — so content that drifted between
# C and its land can only turn full presence into PARTIAL, never the reverse.
#
# VERDICT per commit: PROVEN_LANDED = some candidate with full presence;
# PARTIAL = candidates, none full; NONE = no candidate. The branch is
# PROVEN_LANDED only if every commit is (a land signal alone never suffices —
# D4), NONE if every commit is NONE, else PARTIAL. Zero unlanded commits is
# PROVEN_LANDED: reachability / patch-equivalence on the upstream is the land.
#
# SUPERSESSION — a FOURTH door and a THIRD verdict, never a fourth land
# candidate (plan 2026-09-16-land-evidence-has-no-superseded-arm, D1). Every
# door above asks "did THIS COMMIT land?". For a branch whose files were carried
# forward to the upstream by later DIRECT edits to the same paths, under no PR
# at all, that question is not merely unanswered but structurally unanswerable:
# no merged PR to pair to, no same-subject commit, no coord land stamp. The
# verdict is NONE forever, and the only argument that could ever move such a
# branch was `--adjudicated-landed` — so acting on it recorded "landed" for
# content that never landed. SUPERSEDED is the different word for the different
# claim: "upstream moved PAST this", not "this REACHED upstream".
#   * It creates NO candidate and NEVER sets `land_signal`. A candidate kind
#     yielding PROVEN_LANDED would make `land_signal` true for a branch with no
#     land signal, which is the same defect one layer down.
#   * SUPERSEDED (exit 5) is DECIDED only by a sound test. The general arm
#     requires, for every line C adds that is absent from the upstream tip, an
#     upstream commit LATER than C that REMOVED that exact line from that path
#     (`git log -S`, then a per-commit count comparison). That is an observed
#     upstream DECISION, not an inference from recency — "upstream's copy is
#     newer, therefore discard" would authorize discarding any unlanded work
#     whose file merely got touched later, and is refused. "Later" is
#     max(%at, %ct) against the upstream commit's %ct: a rebase preserves the
#     author date while rewriting the committer date, so %at alone is
#     systematically the less conservative clock.
#   * THE ARM OBSERVES ADDED LINES ONLY, so anything whose claim points the
#     OTHER way is refused outright rather than settled — a commit that REMOVES
#     lines, DELETES a path, changes a MODE, touches a GITLINK or a BINARY blob,
#     or leaves a path at a mode UPSTREAM does not have. Without that the "C
#     added nothing upstream lacks" arm reports a deletion that never landed as
#     safe to discard while upstream still holds the content. Three of these
#     need their own test and do not fall out of the others:
#       - a GITLINK, because `--numstat` prints the `-`/`-` sentinel for BINARY
#         blobs ONLY and reports `1 1` for a submodule pointer, so the binary
#         test does not cover it — the MODE is what is tested;
#       - the branch mode against the UPSTREAM mode, not only against C^:
#         upstream can hold every line C added at a different mode, and the exec
#         bit is then C's only contribution, unlanded;
#       - the removal/deletion refusal is gated on whether the plan-document
#         profile SETTLED this path, never on whether `--corpus` was passed.
#         Keyed on the flag it switched off for the whole RUN, including every
#         path the profile never settles — so a branch deleting a stale plan
#         exited 5 saying it "removes nothing".
#   * SUPERSEDED_CANDIDATE is what everything else the probe cannot settle gets:
#     a structured report (see `supersession` in the JSON) and NO verdict change
#     and NO new exit code — the exit stays whatever the land evidence was
#     (normally 2 NONE), so a caller that has not been taught this arm cannot
#     act on it. A reader decides; the script refuses to guess.
#   * The branch is SUPERSEDED only when EVERY commit is, mirroring the
#     all-or-nothing rule for PROVEN_LANDED.
#   * It runs only from a CLEAN NONE. Any failed land door still forces UNKNOWN
#     first (see below), so supersession can never paper over an outage; and any
#     path whose upstream history could not be read makes that commit UNKNOWN,
#     never SUPERSEDED.
# `--corpus plan-document` adds ONE opt-in profile where the judgment is
# decidable for this fleet's own plan corpus, because `plan-discipline` gives
# plan status a classified vocabulary. A path qualifies when it matches
# `plans/*.md`, exists upstream, the two copies are BYTE-IDENTICAL once each
# side's leading status blockquote (the `>` block before the first `## `) is
# removed, and upstream's status is a STRICTLY later state in the lifecycle
# (draft < vetted < in progress < shipped < superseded/obsolete). An unparseable
# status on either side is UNKNOWN, never "later". Comparing the two bodies
# WHOLE is what lets this profile be exempt from the removed-lines refusal
# above: a line dropped from the body shows up as a body difference, whereas a
# test that only checked where each ADDED line landed would have missed it.
# The profile is tried FIRST and the general arm is a genuine FALLBACK, per
# path — a commit touching `plans/x.md` and `src/y.py` gets the profile's answer
# for the first and the general arm's for the second. It is opt-in precisely so
# the general arm never silently inherits a plan-corpus assumption.
# TWO BOUNDS make the whole-body comparison mean something. An EMPTY body on
# either side is refused: a plan with no `## ` heading deletes to nothing on
# both sides and `cmp` then succeeds vacuously. And the leading `>` run is
# capped at LAND_EVIDENCE_STATUS_BLOCK_MAX_LINES (40) — the profile ignores
# every difference INSIDE that block by design, so its LENGTH is the only thing
# keeping "a status stamp changed" from becoming "substance hid in the one
# region nothing compares".
# AHEAD MERGE COMMITS cap all of that: when `git rev-list --count --merges
# <upstream>..<tip>` is above 0, or cannot be counted, a PROVEN_LANDED becomes
# UNKNOWN (floor PARTIAL) — the zero-unlanded case included. `git cherry`, the
# source of every commit list here, skips merge commits and no test below
# examines one, so an EVIL MERGE's content (added while resolving a conflict)
# would otherwise be discarded with a branch whose every other commit landed.
# UNKNOWN: the verdict is not PROVEN_LANDED and a door that could have supplied
# the missing candidate failed (GitHub, coord, a PR's commit list for pairing, a
# merge commit absent from the object store or unreachable from an upstream that
# may be stale). A failed door never downgrades a land another door proved.
#
# USAGE
#   land-evidence.sh [--json] [--ref <rev>] [--branch <name>] [--upstream <ref>]
#                    [--repo <owner/name>] [--coord-hours N] [--corpus <profile>]
#                    <checkout>
#   --corpus <p>      opt-in supersession profile; the only one is
#                     `plan-document` (see SUPERSESSION above). Omitted, only
#                     the general upstream-removed-later arm can decide.
#   --ref <rev>       evaluate <rev> (a branch, or a refs/wip snapshot) without
#                     checking it out. Default: the checkout's HEAD.
#   --branch <name>   head-branch name for the PR lookups (default: <rev> when it
#                     is a local branch, else the checked-out branch).
#   --upstream <ref>  default origin/HEAD, origin/main, origin/master.
#   --repo <o/n>      GitHub slug (default: parsed from `origin`).
#   --coord-hours N   include_merged window (default: age of the oldest commit
#                     + 48h, capped at $LAND_EVIDENCE_COORD_MAX_HOURS, 720).
#   --json            one object: {"checkout","branch","ref","tip","upstream",
#                     "repo","ahead_merge_commits" (null when uncounted),
#                     "commits":[{"sha","subject","candidates":[{"kind",
#                     "ref","pr","merge_commit","range_diff","added_lines_total",
#                     "added_lines_present","removed_lines_total",
#                     "removed_lines_honored","full_presence","detail"}],
#                     "verdict","supersession":{"verdict","corpus","reason",
#                     "paths":[{"path","upstream_exists","upstream_newest",
#                     "upstream_newest_at","upstream_lines","branch_lines",
#                     "branch_only":[{"line_no","text","disposition"}],
#                     "verdict","reason"}]}|null}],
#                     "evidence_strength","evidence_floor",
#                     "land_signal","error","probes":[{"door","status","detail"}]}
#                     `supersession` is null for a commit the probe did not run
#                     on (i.e. any commit whose land verdict is not NONE).
#                     Doors in probes: upstream, merges, classifier, ancestry, github,
#                     reachability, pairing, same_subject, coord, object_store,
#                     supersession.
# ENV  LAND_EVIDENCE_GH / _CURL / _CLASSIFIER / _LIB_DIR / _PYTHON (overrides),
#      LAND_EVIDENCE_SUPERSEDE_MAX_LINES (default 200): once a commit's
#      branch-only lines reach this many IN TOTAL across the paths it touches,
#      the general arm stops probing and the rest are SUPERSEDED_CANDIDATE — a
#      cumulative bound on the `git log -S` walk (one history walk per line),
#      never a verdict,
#      LAND_EVIDENCE_COORD_URL (default $COORD_HTTP_URL, else
#      https://coord.qontinui.io), LAND_EVIDENCE_NO_MINT=1 (skip the bootstrap
#      mint), LAND_EVIDENCE_TIMEOUT (seconds per network call, default 60).
#      Credential: $COORD_DEVICE_JWT, ~/.qontinui/coord-device-jwt (each only
#      while its exp is >60 s away), then POST /agents/credential with this
#      box's device_id. The token is staged in a mode-600 header file and
#      passed as `curl -H @file` — never on argv.
#
# EXIT 0 PROVEN_LANDED  1 PARTIAL  2 NONE  3 UNKNOWN  4 usage  5 SUPERSEDED
#      5 is ADDITIVE: every existing `-eq 0` test keeps its meaning, and a
#      caller that has not been taught it sees a code it does not recognise
#      rather than a land it does. It is NOT a land — `land_signal` is false on
#      it — so never treat 5 as 0.
# ---- END HELP

set -u

_le_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="${LAND_EVIDENCE_LIB_DIR:-$_le_dir/lib}"
CLASSIFIER="${LAND_EVIDENCE_CLASSIFIER:-$_le_dir/classify-branch-state.sh}"
GH="${LAND_EVIDENCE_GH:-gh}"
CURL="${LAND_EVIDENCE_CURL:-curl}"
COORD_URL="${LAND_EVIDENCE_COORD_URL:-${COORD_HTTP_URL:-https://coord.qontinui.io}}"
NET_TIMEOUT="${LAND_EVIDENCE_TIMEOUT:-60}"; case "$NET_TIMEOUT" in ''|*[!0-9]*) NET_TIMEOUT=60 ;; esac
MAX_HOURS="${LAND_EVIDENCE_COORD_MAX_HOURS:-720}"; case "$MAX_HOURS" in ''|*[!0-9]*) MAX_HOURS=720 ;; esac

if [ -r "$LIB_DIR/git-scope.sh" ]; then . "$LIB_DIR/git-scope.sh"; fi
if declare -F git_scope_strip >/dev/null 2>&1; then
  git_scope_strip
elif [ -n "${GIT_DIR+s}${GIT_WORK_TREE+s}${GIT_COMMON_DIR+s}" ]; then
  echo "land-evidence: FATAL - lib/git-scope.sh is not usable ($LIB_DIR) AND GIT_DIR/GIT_WORK_TREE/GIT_COMMON_DIR is set. Refusing." >&2
  exit 4
fi
if [ -r "$LIB_DIR/native-path.sh" ]; then . "$LIB_DIR/native-path.sh"; fi
if ! declare -F native_path_w >/dev/null 2>&1; then
  if command -v cygpath >/dev/null 2>&1; then
    echo "land-evidence: FATAL - lib/native-path.sh is not usable ($LIB_DIR) on an MSYS box. Refusing." >&2
    exit 4
  fi
  native_path_w() { printf '%s\n' "$1"; }
fi
export MSYS_NO_PATHCONV=1   # `<rev>:<path>` and `<rev>^!` revspecs throughout

usage() { sed -n '2,/^# ---- END HELP/p' "$0" | sed '$d' | sed 's/^#\{0,1\} \{0,1\}//'; }

MODE=human; REF=""; BRANCH_OPT=""; UP_OVERRIDE=""; SLUG=""; HOURS_OPT=""; CHECKOUT=""; CORPUS=""
while [ $# -gt 0 ]; do
  case "$1" in
    --json) MODE=json; shift ;;
    --ref|--branch|--upstream|--repo|--coord-hours|--corpus)
      [ $# -ge 2 ] && [ -n "$2" ] || { echo "land-evidence: $1 needs a value" >&2; exit 4; }
      case "$1" in
        --ref) REF="$2" ;; --branch) BRANCH_OPT="$2" ;; --upstream) UP_OVERRIDE="$2" ;; --repo) SLUG="$2" ;;
        --coord-hours) case "$2" in *[!0-9]*) echo "land-evidence: --coord-hours needs an integer" >&2; exit 4 ;; esac; HOURS_OPT="$2" ;;
        # An unknown profile is a USAGE error, never a silent fall-through to the
        # general arm: a caller that misspells `plan-document` would otherwise be
        # told nothing and get a weaker verdict than it asked for.
        --corpus) case "$2" in plan-document) ;; *) echo "land-evidence: --corpus '$2' is not a profile (known: plan-document)" >&2; exit 4 ;; esac; CORPUS="$2" ;;
      esac; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    --) shift; [ $# -gt 0 ] && { CHECKOUT="$1"; shift; }; [ $# -eq 0 ] || { echo "land-evidence: one checkout at a time" >&2; exit 4; } ;;
    -*) echo "land-evidence: unknown option $1" >&2; exit 4 ;;
    *) [ -z "$CHECKOUT" ] || { echo "land-evidence: one checkout at a time" >&2; exit 4; }; CHECKOUT="$1"; shift ;;
  esac
done
[ -n "$CHECKOUT" ] || { echo "land-evidence: a <checkout> is required (see --help)" >&2; exit 4; }
[ -d "$CHECKOUT" ] || { echo "land-evidence: no such directory: $CHECKOUT" >&2; exit 4; }
CHECKOUT="$(cd "$CHECKOUT" && pwd)"
CO_N="$(native_path_w "$CHECKOUT")"
g() { git --no-optional-locks -c core.quotePath=false -C "$CO_N" "$@"; }
g rev-parse --git-dir >/dev/null 2>&1 || { echo "land-evidence: not a git checkout: $CHECKOUT" >&2; exit 4; }

TMP="$(mktemp -d)" || { echo "land-evidence: cannot mktemp -d" >&2; exit 3; }
chmod 700 "$TMP" 2>/dev/null
trap 'rm -rf "$TMP"' EXIT
np() { if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s' "$1"; fi; }
run_to() { if command -v timeout >/dev/null 2>&1; then timeout "$NET_TIMEOUT" "$@"; else "$@"; fi; }

json_escape() {
  local s="$1"
  s="${s//\\/\\\\}"; s="${s//\"/\\\"}"; s="${s//$'\n'/\\n}"; s="${s//$'\r'/\\r}"; s="${s//$'\t'/\\t}"
  printf '%s' "$s" | tr -d '\000-\010\013\014\016-\037' | json_utf8_clean
}
# Keep the emitted document byte-valid UTF-8. Without this one non-UTF-8 byte
# makes the whole `--json` report unparseable, which is safe in DIRECTION but
# converts a usable audit trail into nothing.
if command -v iconv >/dev/null 2>&1 && printf 'a' | iconv -f utf-8 -t utf-8 -c >/dev/null 2>&1; then
  json_utf8_clean() { iconv -f utf-8 -t utf-8 -c 2>/dev/null; }
else
  json_utf8_clean() { LC_ALL=C tr -d '\200-\377'; }
fi
jstr() { if [ -n "${1:-}" ]; then printf '"%s"' "$(json_escape "$1")"; else printf 'null'; fi; }
jnum() { case "${1:-}" in ''|*[!0-9]*) printf 'null' ;; *) printf '%s' "$1" ;; esac; }

P_DOOR=(); P_STATUS=(); P_DETAIL=()
probe() { P_DOOR+=("$1"); P_STATUS+=("$2"); P_DETAIL+=("$3"); }

C_SHA=(); C_SUBJ=(); C_DATE=(); C_CT=(); C_VERDICT=()
# supersession, per commit index: the verdict word and the D3 report object.
# Empty S_JSON means the probe did not run on that commit, which emits null.
S_VERDICT=(); S_JSON=(); SUPER_FAILED=0
MAX_SUPER_LINES="${LAND_EVIDENCE_SUPERSEDE_MAX_LINES:-200}"
case "$MAX_SUPER_LINES" in ''|*[!0-9]*) MAX_SUPER_LINES=200 ;; esac
# The longest leading `>` run --corpus plan-document will treat as a status
# STAMP. Beyond it the block is a chapter, and the profile would be licensed to
# ignore substance hidden inside the one region it does not compare.
MAX_STATUS_BLOCK_LINES="${LAND_EVIDENCE_STATUS_BLOCK_MAX_LINES:-40}"
case "$MAX_STATUS_BLOCK_LINES" in ''|*[!0-9]*) MAX_STATUS_BLOCK_LINES=40 ;; esac
K_CI=(); K_KIND=(); K_REF=(); K_PR=(); K_MC=(); K_RD=(); K_TOT=(); K_PRES=(); K_RTOT=(); K_RHON=(); K_FULL=(); K_DETAIL=()
UPSTREAM=""; TIP=""; BRANCH=""; STRENGTH=""; FLOOR=""; LAND_SIGNAL=false; FATAL=""; AHEAD_MERGES=""

emit() {
  local i j rc first
  # The ahead-merge cap (VERDICT, above) lives here because every exit runs
  # through emit, so no early return can carry a PROVEN_LANDED past it.
  if [ "$STRENGTH" = PROVEN_LANDED ] && [ "${AHEAD_MERGES:-x}" != 0 ]; then FLOOR=PARTIAL; STRENGTH=UNKNOWN; fi
  # The SAME cap on SUPERSEDED (D6). An ahead merge commit carries content no
  # per-commit test examines — git cherry skips merges, so no commit's patch
  # holds an evil merge's conflict resolution, and the supersession probe walks
  # per-commit patches exactly as the land doors do. The floor is NONE rather
  # than PARTIAL because supersession never produced a land candidate.
  if [ "$STRENGTH" = SUPERSEDED ] && [ "${AHEAD_MERGES:-x}" != 0 ]; then FLOOR=NONE; STRENGTH=UNKNOWN; fi
  case "$STRENGTH" in PROVEN_LANDED) rc=0 ;; PARTIAL) rc=1 ;; NONE) rc=2 ;; SUPERSEDED) rc=5 ;; *) rc=3; STRENGTH=UNKNOWN ;; esac
  if [ "$MODE" = json ]; then
    printf '{"checkout":%s,"branch":%s,"ref":%s,"tip":%s,"upstream":%s,"repo":%s,"ahead_merge_commits":%s,"commits":[' \
      "$(jstr "$CHECKOUT")" "$(jstr "$BRANCH")" "$(jstr "${REF:-HEAD}")" "$(jstr "$TIP")" "$(jstr "$UPSTREAM")" "$(jstr "$SLUG")" "$(jnum "$AHEAD_MERGES")"
    for i in "${!C_SHA[@]}"; do
      [ "$i" -gt 0 ] && printf ','
      printf '{"sha":%s,"subject":%s,"candidates":[' "$(jstr "${C_SHA[$i]}")" "$(jstr "${C_SUBJ[$i]}")"
      first=1
      for j in "${!K_CI[@]}"; do
        [ "${K_CI[$j]}" = "$i" ] || continue
        [ "$first" = 1 ] || printf ','; first=0
        printf '{"kind":%s,"ref":%s,"pr":%s,"merge_commit":%s,"range_diff":%s,"added_lines_total":%s,"added_lines_present":%s,"removed_lines_total":%s,"removed_lines_honored":%s,"full_presence":%s,"detail":%s}' \
          "$(jstr "${K_KIND[$j]}")" "$(jstr "${K_REF[$j]}")" "$(jnum "${K_PR[$j]}")" "$(jstr "${K_MC[$j]}")" \
          "$(jstr "${K_RD[$j]}")" "$(jnum "${K_TOT[$j]}")" "$(jnum "${K_PRES[$j]}")" \
          "$(jnum "${K_RTOT[$j]}")" "$(jnum "${K_RHON[$j]}")" "${K_FULL[$j]}" "$(jstr "${K_DETAIL[$j]}")"
      done
      printf '],"verdict":%s,"supersession":%s}' "$(jstr "${C_VERDICT[$i]:-NONE}")" "${S_JSON[$i]:-null}"
    done
    printf '],"evidence_strength":%s,"evidence_floor":%s,"land_signal":%s,"error":%s,"probes":[' \
      "$(jstr "$STRENGTH")" "$(jstr "${FLOOR:-$STRENGTH}")" "$LAND_SIGNAL" "$(jstr "$FATAL")"
    for i in "${!P_DOOR[@]}"; do
      [ "$i" -gt 0 ] && printf ','
      printf '{"door":%s,"status":%s,"detail":%s}' "$(jstr "${P_DOOR[$i]}")" "$(jstr "${P_STATUS[$i]}")" "$(jstr "${P_DETAIL[$i]}")"
    done
    printf ']}\n'
  else
    printf 'land-evidence: %s  ref=%s branch=%s upstream=%s repo=%s ahead_merges=%s\n' "$CHECKOUT" "${REF:-HEAD}" "${BRANCH:-<none>}" "${UPSTREAM:-<none>}" "${SLUG:-<none>}" "${AHEAD_MERGES:-uncounted}"
    [ -n "$FATAL" ] && printf '  ERROR: %s\n' "$FATAL"
    for i in "${!C_SHA[@]}"; do
      printf '  %s %s  [%s]\n' "${C_SHA[$i]:0:12}" "${C_SUBJ[$i]}" "${C_VERDICT[$i]:-NONE}"
      for j in "${!K_CI[@]}"; do
        [ "${K_CI[$j]}" = "$i" ] || continue
        printf '      %-20s %s%s added %s/%s removed %s/%s  %s\n' "${K_KIND[$j]}" "${K_REF[$j]:0:12}" "$([ -n "${K_PR[$j]}" ] && printf ' (#%s)' "${K_PR[$j]}")" \
          "${K_PRES[$j]}" "${K_TOT[$j]}" "${K_RHON[$j]}" "${K_RTOT[$j]}" "${K_RD[$j]:0:160}"
      done
      [ -n "${S_VERDICT[$i]:-}" ] && printf '      %-20s %s\n' "supersession" "${S_VERDICT[$i]}"
    done
    for i in "${!P_DOOR[@]}"; do printf '  probe %-12s %-10s %s\n' "${P_DOOR[$i]}" "${P_STATUS[$i]}" "${P_DETAIL[$i]}"; done
    printf '\n  EVIDENCE: %s%s  (land signal: %s)\n' "$STRENGTH" "$([ -n "$FLOOR" ] && [ "$FLOOR" != "$STRENGTH" ] && printf ' (floor %s)' "$FLOOR")" "$LAND_SIGNAL"
  fi
  exit "$rc"
}
fatal() { FATAL="$1"; STRENGTH=UNKNOWN; emit; }

# ---------------------------------------------------------------------------
# upstream, tip, branch
if [ -n "$UP_OVERRIDE" ]; then
  g rev-parse -q --verify "$UP_OVERRIDE^{commit}" >/dev/null 2>&1 && UPSTREAM="$UP_OVERRIDE"
else
  _r="$(g symbolic-ref -q refs/remotes/origin/HEAD 2>/dev/null)"
  if [ -n "$_r" ] && g rev-parse -q --verify "$_r^{commit}" >/dev/null 2>&1; then UPSTREAM="${_r#refs/remotes/}"
  else for _r in origin/main origin/master; do
    g rev-parse -q --verify "refs/remotes/$_r^{commit}" >/dev/null 2>&1 && { UPSTREAM="$_r"; break; }
  done; fi
fi
[ -n "$UPSTREAM" ] || { probe upstream failed "no upstream ref resolves (${UP_OVERRIDE:-origin/HEAD, origin/main, origin/master})"; fatal "no upstream ref"; }

if [ -n "$REF" ]; then
  TIP="$(g rev-parse -q --verify "$REF^{commit}" 2>/dev/null)" || fatal "--ref '$REF' is not a commit here"
  BRANCH="$BRANCH_OPT"
  if [ -z "$BRANCH" ]; then
    if g show-ref --verify -q "refs/heads/$REF" 2>/dev/null; then BRANCH="$REF"
    else case "$REF" in refs/heads/*) BRANCH="${REF#refs/heads/}" ;; esac; fi
  fi
else
  TIP="$(g rev-parse -q --verify "HEAD^{commit}" 2>/dev/null)" || fatal "HEAD does not resolve"
  BRANCH="${BRANCH_OPT:-$(g symbolic-ref -q --short HEAD 2>/dev/null)}"
fi
g merge-base "$UPSTREAM" "$TIP" >/dev/null 2>&1 || { probe upstream failed "no merge base between $UPSTREAM and ${TIP:0:12}"; fatal "unrelated histories"; }

# ---------------------------------------------------------------------------
# ahead merge commits: counted before any evidence, because none of it looks at
# them (see VERDICT; emit applies the cap)
AHEAD_MERGES="$(g rev-list --count --merges "$UPSTREAM..$TIP" 2>/dev/null)"
case "$AHEAD_MERGES" in ''|*[!0-9]*) AHEAD_MERGES="" ;; esac
if [ -z "$AHEAD_MERGES" ]; then
  probe merges failed "could not count the merge commits in $UPSTREAM..${TIP:0:12}; PROVEN_LANDED is unavailable until they are counted"
elif [ "$AHEAD_MERGES" -gt 0 ]; then
  _ml="$(g rev-list --merges "$UPSTREAM..$TIP" 2>/dev/null | head -5 | cut -c1-12 | paste -sd' ' -)"
  probe merges unexamined "$AHEAD_MERGES merge commit(s) in $UPSTREAM..${TIP:0:12} (${_ml}$([ "$AHEAD_MERGES" -gt 5 ] && printf ' ...')): ahead merge commits carry content no per-commit test examines (git cherry skips them; an evil merge's conflict resolution is in no commit's patch), so PROVEN_LANDED is unavailable"
else
  probe merges ok "0 merge commits in $UPSTREAM..${TIP:0:12}"
fi

# ---------------------------------------------------------------------------
# unlanded commits
cherry_unlanded() {
  g cherry "$UPSTREAM" "$TIP" > "$TMP/cherry" 2>/dev/null || return 1
  sed -n 's/^+ \([0-9a-f]\{40,64\}\)$/\1/p' "$TMP/cherry"
}
# The classifier's entries are "<sha> <subject>" (git cherry -v). Walk the array
# as JSON, so a `]` or an escaped quote inside a subject does not end it early
# (which silently dropped every later commit), and keep each entry's LEADING
# field only: a subject may itself hold a sha ("Revert <sha>"), and that is no
# commit of this branch. An entry whose leading field is not a full sha makes
# the whole list unusable (return 1, and git cherry supplies it) rather than
# quietly shorter.
classifier_unlanded() { # <classifier json> -> one sha per line, or 1
  local e s
  printf '%s' "$1" | awk '
    { buf = buf $0 }
    END {
      k = index(buf, "\"unlanded_commits\":[")
      if (!k) exit 1
      s = substr(buf, k + 20); ins = 0; esc = 0; cur = ""
      for (p = 1; p <= length(s); p++) {
        c = substr(s, p, 1)
        if (ins) {
          if (esc) { esc = 0; cur = cur c }
          else if (c == "\\") { esc = 1; cur = cur c }
          else if (c == "\"") { ins = 0; print cur; cur = "" }
          else cur = cur c
        }
        else if (c == "\"") ins = 1
        else if (c == "]") exit 0
      }
      exit 1
    }' > "$TMP/clist.raw" || return 1
  while IFS= read -r e; do
    s="${e%% *}"   # the LEADING field -- never a sha that appears later, in the subject
    case "$s" in ''|*[!0-9a-f]*) return 1 ;; esac
    [ "${#s}" -eq 40 ] || [ "${#s}" -eq 64 ] || return 1
    printf '%s\n' "$s"
  done < "$TMP/clist.raw"
}
UNL=""
if [ -z "$REF" ] && [ -r "$CLASSIFIER" ]; then
  cjson="$(bash "$CLASSIFIER" --json --upstream "$UPSTREAM" "$CHECKOUT" 2>/dev/null)"; crc=$?
  cverdict="$(printf '%s' "$cjson" | grep -o '"verdict":"[A-Z_]*"' | head -1 | sed 's/.*:"//; s/"$//')"
  cbad=""
  clist="$(classifier_unlanded "$cjson")" || { clist=""
    case "$cjson" in *'"unlanded_commits":'*) cbad=" (its unlanded_commits did not parse as \"<sha> <subject>\" entries)" ;; esac; }
  if [ "$cverdict" = LANDED_DUPLICATE ]; then
    probe classifier ok "LANDED_DUPLICATE (exit $crc): nothing on this branch is missing from $UPSTREAM"
  elif [ -n "$clist" ]; then
    UNL="$clist"; probe classifier ok "$cverdict (exit $crc): $(printf '%s\n' "$clist" | wc -l | tr -d ' ') unlanded commit(s)"
  else
    UNL="$(cherry_unlanded)" || fatal "git cherry $UPSTREAM $TIP failed"
    probe classifier fallback "${cverdict:-no verdict} (exit $crc) carried no usable unlanded list${cbad:- (a dirty tree short-circuits it)}; commits from git cherry, its own patch-id primitive"
  fi
else
  UNL="$(cherry_unlanded)" || fatal "git cherry $UPSTREAM $TIP failed"
  if [ -n "$REF" ]; then probe classifier skipped "--ref: unlanded commits from git cherry $UPSTREAM $REF (the classifier's patch-id primitive; no checkout needed)"
  else probe classifier failed "classifier not readable at $CLASSIFIER; used git cherry"; fi
fi
for s in $UNL; do
  C_SHA+=("$s"); C_SUBJ+=("$(g log -1 --format=%s "$s" 2>/dev/null)"); C_DATE+=("$(g log -1 --format=%at "$s" 2>/dev/null)")
  # The COMMITTER date as well. The same_subject door wants the AUTHOR date (it
  # asks "was this written around then"), but supersession asks "did upstream
  # decide against this AFTER it existed", and a rebase/cherry-pick/`git am`
  # preserves %at while rewriting %ct -- so %at is systematically the EARLIER
  # and therefore LESS conservative clock, and using it admits upstream removals
  # that predate the work. Supersession uses max(%at, %ct).
  C_CT+=("$(g log -1 --format=%ct "$s" 2>/dev/null)")
done

if [ "${#C_SHA[@]}" -eq 0 ]; then
  STRENGTH=PROVEN_LANDED; LAND_SIGNAL=true
  probe ancestry ok "0 unlanded commits: every single-parent commit on ${TIP:0:12} is on $UPSTREAM or patch-equivalent to one there"
  emit   # which caps this at UNKNOWN when merge commits are ahead
fi

# ---------------------------------------------------------------------------
# candidate evaluation
EMPTY_TREE=4b825dc642cb6eb9a060e54bf8d69288fbee4904
OBJ_MISSING=0
blob_or_empty() { # <rev:path> <out> -> 0 and the blob's bytes when it is a blob, else 1 and an empty file
  if [ "$(g cat-file -t "$1" 2>/dev/null)" = blob ]; then
    g cat-file blob "$1" > "$2" 2>/dev/null || { : > "$2"; return 1; }
    return 0
  fi
  : > "$2"; return 1
}
entry_of() { # <rev> <path> -> "<mode> <oid>" of that exact tree entry ("" when absent), or 1 when unreadable
  local out
  out="$(g ls-tree "$1" -- ":(top,literal)$2" 2>/dev/null)" || return 1
  out="${out%%$'\t'*}"
  [ -z "$out" ] || printf '%s %s' "${out%% *}" "${out##* }"
}
add_candidate() { # <ci> <kind> <commit> <pr> <merge_commit> <detail>
  local ci="$1" kind="$2" c="$3" j l par rec a d path rd="" crange at ap rt rh pe le ce pm lm mp=""
  local atot=0 apres=0 rtot=0 rhon=0 mism=0
  for j in "${!K_CI[@]}"; do [ "${K_CI[$j]}" = "$ci" ] && [ "${K_KIND[$j]}" = "$kind" ] && [ "${K_REF[$j]}" = "$c" ] && return 0; done
  # An absent object is not a land signal: record it, never add it as a candidate.
  if ! g cat-file -e "$c^{commit}" 2>/dev/null; then
    OBJ_MISSING=1; probe object_store failed "$kind candidate ${c:0:12} is not in the local object store (fetch origin); not counted as a land signal"
    return 0
  fi
  l="${C_SHA[$ci]}"
  K_CI+=("$ci"); K_KIND+=("$kind"); K_REF+=("$c"); K_PR+=("$4"); K_MC+=("$5"); K_DETAIL+=("$6")
  par="$(g rev-parse -q --verify "$l^1" 2>/dev/null)" || par="$EMPTY_TREE"
  g diff --no-renames --ignore-submodules=none --numstat -z "$par" "$l" > "$TMP/numstat" 2>/dev/null
  while IFS= read -r -d '' rec; do
    a="${rec%%$'\t'*}"; rec="${rec#*$'\t'}"; d="${rec%%$'\t'*}"; path="${rec#*$'\t'}"
    # the path's tree entry ("<mode> <oid>", "" when absent) before C, after C, and at the candidate
    if ! pe="$(entry_of "$par" "$path")" || ! le="$(entry_of "$l" "$path")" || ! ce="$(entry_of "$c" "$path")"; then
      mism=$((mism + 1)); mp="$mp $path (tree entry unreadable);"; continue
    fi
    pm="${pe%% *}"; lm="${le%% *}"
    if [ "$lm" = 160000 ] || [ "$pm" = 160000 ] || { [ "$a" = 0 ] && [ "$d" = 0 ]; }; then
      # a gitlink, or a change with no lines to count (a mode change, an empty file
      # added): the TREE ENTRY itself -- mode + object id -- must be as C left it
      if [ "$ce" != "$le" ]; then mism=$((mism + 1)); mp="$mp $path (tree entry);"; fi
      continue
    fi
    # a mode C sets on a path it keeps (an add included) must be the candidate's too
    if [ -n "$lm" ] && [ "$lm" != "$pm" ] && [ "${ce%% *}" != "$lm" ]; then mism=$((mism + 1)); mp="$mp $path (mode);"; fi
    if [ "$a" = "-" ]; then
      # binary: blob-for-blob (a deleted binary must be absent at the candidate too)
      atot=$((atot + 1))
      [ "${ce##* }" = "${le##* }" ] && apres=$((apres + 1))
    else
      g diff --no-renames --no-color --no-ext-diff --no-textconv -U0 "$par" "$l" -- ":(top,literal)$path" 2>/dev/null \
        | awk '/^diff --git /{h=0; next} /^@@/{h=1; next} h && /^[+-]/{print}' > "$TMP/delta"
      blob_or_empty "$l:$path" "$TMP/post" || {
        # C deletes the file: the candidate must not hold it, or hold it empty
        blob_or_empty "$c:$path" "$TMP/cand" && [ -s "$TMP/cand" ] && { mism=$((mism + 1)); mp="$mp $path (a deleted file still holding content);"; }; }
      blob_or_empty "$c:$path" "$TMP/cand"
      # MULTISET, per line: an added L needs count(cand,L) >= count(post,L); a
      # removed R needs count(cand,R) <= count(post,R). present/honored count the
      # copies the candidate accounts for, so a shortfall shows as a number.
      read -r at ap rt rh < <(awk '
        FILENAME == ARGV[1] { cand[$0]++; next }
        FILENAME == ARGV[2] { post[$0]++; next }
        { x = substr($0, 2); if (substr($0, 1, 1) == "+") add[x]++; else rem[x]++ }
        END {
          for (x in add) { at += add[x]; def = post[x] - cand[x]; if (def < 0) def = 0; p = add[x] - def; if (p < 0) p = 0; ap += p }
          for (x in rem) { rt += rem[x]; exc = cand[x] - post[x]; if (exc < 0) exc = 0; h = rem[x] - exc; if (h < 0) h = 0; rh += h }
          print at + 0, ap + 0, rt + 0, rh + 0
        }' "$TMP/cand" "$TMP/post" "$TMP/delta")
      atot=$((atot + at)); apres=$((apres + ap)); rtot=$((rtot + rt)); rhon=$((rhon + rh))
    fi
  done < "$TMP/numstat"
  if [ "$par" = "$EMPTY_TREE" ]; then rd="n/a (root commit)"
  else
    if g rev-parse -q --verify "$c^2" >/dev/null 2>&1; then crange="$c^1..$c^2"; else crange="$c^!"; fi
    rd="$(run_to git --no-optional-locks -C "$CO_N" range-diff --no-color "$l^!" "$crange" 2>/dev/null \
          | grep -E '^ *[0-9-]+: +[0-9a-f-]+ [=!<>] ' | sed 's/  */ /g' | head -5 | paste -sd'|' -)"
    [ -n "$rd" ] || rd="range-diff produced no pairing"
  fi
  [ "$mism" -gt 0 ] && rd="$rd; $mism path(s) not as the local commit left them:${mp%;}"
  K_RD+=("$rd"); K_TOT+=("$atot"); K_PRES+=("$apres"); K_RTOT+=("$rtot"); K_RHON+=("$rhon")
  if [ "$mism" -eq 0 ] && [ "$apres" -eq "$atot" ] && [ "$rhon" -eq "$rtot" ]; then K_FULL+=(true); else K_FULL+=(false); fi
}

# --- PR-keyed candidates: reachability, then pairing -----------------------
UP_BRANCH="${UPSTREAM#refs/remotes/}"; UP_BRANCH="${UP_BRANCH#*/}"
PAIR_FAILED=0; UP_STALE=0
reachable_land() { # <kind> <pr> <merge_commit> <pr base or ""> -> 0 when it may be a candidate
  local kind="$1" n="$2" mc="$3" base="$4"
  if ! g cat-file -e "$mc^{commit}" 2>/dev/null; then
    OBJ_MISSING=1; probe reachability failed "$kind PR #$n: merge commit ${mc:0:12} is not in the local object store (fetch $UPSTREAM); not a land signal until it can be checked"
    return 1
  fi
  g merge-base --is-ancestor "$mc" "$UPSTREAM" 2>/dev/null && return 0
  if [ -n "$base" ] && [ "$base" != "$UP_BRANCH" ]; then
    probe reachability rejected "$kind PR #$n: merge commit ${mc:0:12} is not reachable from $UPSTREAM (PR base '$base'); a merge into another branch is not a land"
  else
    UP_STALE=1; probe reachability failed "$kind PR #$n: merge commit ${mc:0:12} is not reachable from $UPSTREAM${base:+ (PR base '$base')}; the upstream ref may be stale (fetch). Not a land signal"
  fi
  return 1
}
pr_commit_list() { # <pr> -> 0 and $TMP/prc.<pr> ("<oid>\t<subject>" lines), or 1
  local n="$1" f="$TMP/prc.$1" oid hl b1 s
  [ -e "$f.ok" ] && return 0; [ -e "$f.fail" ] && return 1
  if ! run_to "$GH" pr view "$n" --repo "$SLUG" --json commits \
       --jq '.commits[] | [.oid, (.messageHeadline // ""), ((((.messageBody // "") | split("\n"))[0]) // "")] | @tsv' \
       > "$f.raw" 2> "$f.err"; then
    : > "$f.fail"; return 1
  fi
  : > "$f"
  while IFS=$'\x1f' read -r oid hl b1; do
    [ -n "$oid" ] || continue
    s="$(g log -1 --format=%s "$oid" 2>/dev/null)"   # the object's own subject when it is here
    if [ -z "$s" ]; then
      s="$hl"   # GitHub cuts a long headline with an ellipsis and carries the rest into the body
      case "$hl" in *…) case "$b1" in …*) s="${hl%…}${b1#…}" ;; esac ;; esac
    fi
    printf '%s\t%s\n' "$oid" "$s" >> "$f"
  done < <(tr -d '\r' < "$f.raw" | tr '\t' '\037')
  : > "$f.ok"
}
pair_how() { # <ci> <pr> -> prints sha|subject and 0 when commit <ci> belongs to PR <pr>
  local f="$TMP/prc.$2"
  O="${C_SHA[$1]}" awk -F'\t' '$1 == ENVIRON["O"] {f = 1} END {exit !f}' "$f" && { printf sha; return 0; }
  [ -n "${C_SUBJ[$1]}" ] && S="${C_SUBJ[$1]}" awk -F'\t' '$2 == ENVIRON["S"] {f = 1} END {exit !f}' "$f" && { printf subject; return 0; }
  return 1
}
attach_pr_candidate() { # <kind> <pr> <merge_commit> <pr base or ""> <detail>
  local kind="$1" n="$2" mc="$3" base="$4" det="$5" i how paired="" unpaired=""
  reachable_land "$kind" "$n" "$mc" "$base" || return 0
  if ! pr_commit_list "$n"; then
    PAIR_FAILED=1
    probe pairing failed "$kind PR #$n: its commit list did not read ($(head -1 "$TMP/prc.$n.err" 2>/dev/null | cut -c1-160)); merge commit ${mc:0:12} is a candidate for no commit"
    return 0
  fi
  for i in "${!C_SHA[@]}"; do
    if how="$(pair_how "$i" "$n")"; then
      add_candidate "$i" "$kind" "$mc" "$n" "$mc" "$det; paired by $how"; paired="$paired ${C_SHA[$i]:0:12}($how)"
    else unpaired="$unpaired ${C_SHA[$i]:0:12}"; fi
  done
  probe pairing "$([ -n "$paired" ] && echo ok || echo none)" \
    "$kind PR #$n ($(grep -c . "$TMP/prc.$n") commit(s)): paired:${paired:- none}; not paired (neither sha nor subject among the PR's commits):${unpaired:- none}"
}

compute_strength() {
  local i j any_proven all_proven=1 all_none=1 has
  LAND_SIGNAL=false
  for i in "${!C_SHA[@]}"; do
    any_proven=0; has=0
    for j in "${!K_CI[@]}"; do
      [ "${K_CI[$j]}" = "$i" ] || continue
      has=1; LAND_SIGNAL=true
      [ "${K_FULL[$j]}" = true ] && any_proven=1
    done
    if [ "$any_proven" = 1 ]; then C_VERDICT[$i]=PROVEN_LANDED; all_none=0
    elif [ "$has" = 1 ]; then C_VERDICT[$i]=PARTIAL; all_proven=0; all_none=0
    else C_VERDICT[$i]=NONE; all_proven=0; fi
  done
  if [ "$all_proven" = 1 ]; then STRENGTH=PROVEN_LANDED
  elif [ "$all_none" = 1 ]; then STRENGTH=NONE
  else STRENGTH=PARTIAL; fi
}

# ---------------------------------------------------------------------------
# door 1: GitHub
if [ -z "$SLUG" ]; then
  _u="$(g remote get-url origin 2>/dev/null)"
  SLUG="$(printf '%s' "$_u" | sed -n 's#^.*github\.com[:/]\([^/]*\)/\([^/]*\)$#\1/\2#p' | sed 's/\.git$//')"
fi
PR_NUM=(); PR_STATE=(); PR_MC=(); PR_BASE=(); GH_FAILED=0; CLOSED_UNMERGED=0
pr_base() { local k; for k in "${!PR_NUM[@]}"; do [ "${PR_NUM[$k]}" = "$1" ] && { printf '%s' "${PR_BASE[$k]}"; return; }; done; }
if [ -z "$BRANCH" ]; then
  probe github skipped "no head-branch name (detached HEAD or a non-branch --ref without --branch)"
elif [ -z "$SLUG" ]; then
  GH_FAILED=1; probe github failed "origin is not a GitHub URL and no --repo was given"
elif ! command -v "$GH" >/dev/null 2>&1; then
  GH_FAILED=1; probe github failed "'$GH' is not on PATH"
else
  if run_to "$GH" pr list --repo "$SLUG" --head "$BRANCH" --state all --limit 50 \
       --json number,state,mergedAt,mergeCommit,baseRefName \
       --jq '.[] | [(.number|tostring), .state, (.mergedAt // ""), (.mergeCommit.oid // ""), (.baseRefName // "")] | @tsv' \
       > "$TMP/gh.tsv" 2> "$TMP/gh.err"; then
    _d=""; _merged=()
    # Split on \x1f, not tab: tab is IFS whitespace, so runs of it collapse and an
    # empty field would shift every later one left.
    while IFS=$'\x1f' read -r n st ma mc base; do
      [ -n "$n" ] || continue
      PR_NUM+=("$n"); PR_STATE+=("$st"); PR_MC+=("$mc"); PR_BASE+=("$base")
      _d="$_d #$n $st${base:+ base $base}${mc:+ merge_commit ${mc:0:12}};"
      if [ "$st" = MERGED ] && [ -n "$mc" ]; then _merged+=("$n"$'\x1f'"$mc"$'\x1f'"$base"$'\x1f'"$ma")
      elif [ "$st" = CLOSED ]; then CLOSED_UNMERGED=1; fi
    done < <(tr -d '\r' < "$TMP/gh.tsv" | tr '\t' '\037')
    probe github ok "${#PR_NUM[@]} PR(s) for head $BRANCH on $SLUG:${_d:- none}"
    for _m in ${_merged[@]+"${_merged[@]}"}; do
      IFS=$'\x1f' read -r n mc base ma <<< "$_m"
      attach_pr_candidate github_merged_pr "$n" "$mc" "$base" "GitHub MERGED at $ma"
    done
  else
    GH_FAILED=1; probe github failed "gh pr list failed: $(head -1 "$TMP/gh.err" | cut -c1-200)"
  fi
fi

# door 2: same subject on the upstream
_oldest=""; for d in "${C_DATE[@]}"; do [ -n "$d" ] && { [ -z "$_oldest" ] || [ "$d" -lt "$_oldest" ]; } && _oldest="$d"; done
if [ -n "$_oldest" ] && g log "$UPSTREAM" --since="@$((_oldest - 86400))" --format="%H%x1f%ct%x1f%s" > "$TMP/uplog" 2>/dev/null; then
  _n=0
  for i in "${!C_SHA[@]}"; do
    while IFS= read -r h; do
      [ -n "$h" ] && [ "$h" != "${C_SHA[$i]}" ] || continue
      add_candidate "$i" same_subject_on_main "$h" "" "" "identical subject on $UPSTREAM"; _n=$((_n + 1))
    done < <(awk -F $'\x1f' -v s="${C_SUBJ[$i]}" -v t="$(( ${C_DATE[$i]:-0} - 86400 ))" '$3 == s && $2 >= t {print $1}' "$TMP/uplog" | head -5)
  done
  probe same_subject ok "$_n same-subject commit(s) on $UPSTREAM since a day before the oldest unlanded commit"
else
  probe same_subject failed "git log $UPSTREAM failed"
fi
compute_strength

# door 3: coord land stamp — only when it can change the answer
jwt_shaped() { case "$1" in "" | *[!A-Za-z0-9._-]*) return 1 ;; esac; [ "$(printf '%s' "$1" | tr -cd '.' | wc -c | tr -d ' ')" = 2 ]; }
jwt_exp_future() {
  local seg exp
  seg="$(printf '%s' "$1" | cut -d. -f2 | tr '_-' '/+')"
  case $(( ${#seg} % 4 )) in 2) seg="$seg==" ;; 3) seg="$seg=" ;; esac
  exp="$(printf '%s' "$seg" | base64 --decode 2>/dev/null | grep -o '"exp"[[:space:]]*:[[:space:]]*[0-9]*' | grep -o '[0-9]*$')"
  [ -n "$exp" ] && [ $((exp - $(date +%s))) -gt 60 ]
}
# jwt-cascade-selection: both STATIC sources are gated on SHAPE and on `exp`
# BEFORE selection -- `jwt_shaped` && `jwt_exp_future` (exp more than 60 s away)
# at both rungs below -- so an expired or malformed $COORD_DEVICE_JWT falls
# THROUGH to ~/.qontinui/coord-device-jwt instead of shadowing it (#366: the
# first USABLE source wins, not the first with bytes). The predicate is spelled
# `jwt_exp_future` rather than `jwt_fresh`, which is the only reason check #E's
# FRESH_RE does not see it. The third rung is not static: it mints via
# /agents/credential when neither static rung yielded a usable token (absent,
# unreadable, malformed, exp-less, or exp 60 s away or less), unless
# LAND_EVIDENCE_NO_MINT=1 or no device_id resolves from $QONTINUI_MACHINE_ID or
# ~/.qontinui/machine.json -- so a stale token cannot shadow the mint either.
# `skip_env=1` is the caller's rejection signal (coord answered 401 to the env
# token) and starts one rung down.
stage_bearer() { # [skip_env] -> prints source; writes $TMP/bearer.hdr (0600)
  local jwt="" src=none home="${HOME:-${USERPROFILE:-}}" e f dev body code
  rm -f "$TMP/bearer.hdr"
  e="$(printf '%s' "${COORD_DEVICE_JWT:-}" | tr -d '[:space:]')"
  if [ "${1:-0}" != 1 ] && jwt_shaped "$e" && jwt_exp_future "$e"; then jwt="$e"; src=env
  elif [ -n "$home" ] && [ -r "$home/.qontinui/coord-device-jwt" ]; then
    f="$(tr -d '[:space:]' < "$home/.qontinui/coord-device-jwt" 2>/dev/null)"
    jwt_shaped "$f" && jwt_exp_future "$f" && { jwt="$f"; src=file; }
  fi
  # $TMP/mint.note says what became of the mint rung, so the coord door's
  # failure detail can tell "coord refused the mint" from "this box has no
  # device_id" from "the caller opted out" -- three different remedies. A file
  # rather than a variable because this function runs in a $(...) subshell.
  rm -f "$TMP/mint.note"
  if [ -z "$jwt" ] && [ "${LAND_EVIDENCE_NO_MINT:-0}" = 1 ]; then
    printf 'skipped (LAND_EVIDENCE_NO_MINT=1)' > "$TMP/mint.note"
  elif [ -z "$jwt" ]; then
    dev="${QONTINUI_MACHINE_ID:-}"
    [ -z "$dev" ] && [ -n "$home" ] && [ -r "$home/.qontinui/machine.json" ] && \
      dev="$(grep -o '"\(device_id\|machine_id\)"[[:space:]]*:[[:space:]]*"[^"]*"' "$home/.qontinui/machine.json" | head -1 | sed 's/.*:[[:space:]]*"//; s/"$//')"
    if [ -n "$dev" ]; then
      ( umask 077; : > "$TMP/mint.json" )
      code="$(run_to "$CURL" -sS -o "$(np "$TMP/mint.json")" -w '%{http_code}' --connect-timeout 10 -m "$NET_TIMEOUT" \
             -X POST "$COORD_URL/agents/credential" -H 'Content-Type: application/json' -d "{\"device_id\":\"$dev\"}" 2>/dev/null)"
      if [ "$code" = 200 ]; then
        f="$(grep -o '"\(token\|agent_jwt\|jwt\|access_token\)"[[:space:]]*:[[:space:]]*"[^"]*"' "$TMP/mint.json" | head -1 | sed 's/.*:[[:space:]]*"//; s/"$//')"
        if jwt_shaped "$f"; then jwt="$f"; src=mint
        else printf 'POST /agents/credential answered HTTP 200 without a jwt-shaped token in its body' > "$TMP/mint.note"; fi
      else
        case "$code" in
          ''|000) printf 'POST /agents/credential got no answer (curl 000 / timeout)' > "$TMP/mint.note" ;;
          *)      printf 'POST /agents/credential answered HTTP %s' "$code" > "$TMP/mint.note" ;;
        esac
      fi
      rm -f "$TMP/mint.json"
    else
      printf 'not asked: no device_id from $QONTINUI_MACHINE_ID or ~/.qontinui/machine.json' > "$TMP/mint.note"
    fi
  fi
  [ -n "$jwt" ] && ( umask 077; printf 'Authorization: Bearer %s\n' "$jwt" > "$TMP/bearer.hdr" )
  printf '%s' "$src"
}
resolve_python() {
  local c
  for c in "${LAND_EVIDENCE_PYTHON:-}" python3 python; do
    [ -n "$c" ] && command -v "$c" >/dev/null 2>&1 || continue
    "$c" -c 'import sys; sys.exit(0 if sys.version_info[0] == 3 else 1)' >/dev/null 2>&1 && { printf '%s' "$c"; return 0; }
  done
  return 1
}
COORD_FAILED=0
if [ "$STRENGTH" = PROVEN_LANDED ]; then
  probe coord skipped "not needed: the land is already proven by another door"
elif [ -z "$BRANCH" ] || [ -z "$SLUG" ]; then
  probe coord skipped "no branch/repo to match a coord row against"
elif [ "$GH_FAILED" = 0 ] && [ "$CLOSED_UNMERGED" = 0 ]; then
  probe coord skipped "GitHub listed no CLOSED-unmerged PR for $BRANCH, so there is no fast-forward land for coord to stamp"
elif ! PY="$(resolve_python)"; then
  COORD_FAILED=1; probe coord failed "no Python 3 interpreter to parse the coord listing"
else
  if [ -n "$HOURS_OPT" ]; then hours="$HOURS_OPT"
  else hours=$(( ( $(date +%s) - _oldest ) / 3600 + 48 )); [ "$hours" -lt 24 ] && hours=24; fi
  clamped=0; [ "$hours" -gt "$MAX_HOURS" ] && { hours="$MAX_HOURS"; clamped=1; }
  src="$(stage_bearer)"
  url="$COORD_URL/pr-merge/prs?include_merged=$hours"
  coord_get() {
    : > "$TMP/coord.json"
    local hdr=(); [ -r "$TMP/bearer.hdr" ] && hdr=(-H "@$(np "$TMP/bearer.hdr")")
    code="$(run_to "$CURL" -sS -o "$(np "$TMP/coord.json")" -w '%{http_code}' --connect-timeout 10 -m "$NET_TIMEOUT" ${hdr[@]+"${hdr[@]}"} "$url" 2>"$TMP/coord.err")"; crc=$?
  }
  if [ "$src" = none ]; then
    _mn="$(cat "$TMP/mint.note" 2>/dev/null)"
    COORD_FAILED=1; probe coord failed "no usable device JWT (env, ~/.qontinui/coord-device-jwt, bootstrap mint all missed${_mn:+; mint: $_mn})"
  else
    coord_get
    if [ "$code" = 401 ] && [ "$src" = env ]; then
      src="$(stage_bearer 1)"; [ "$src" = none ] || coord_get
    fi
    case "$url" in file://*) [ "$crc" -eq 0 ] && code=200 ;; esac
    if [ "$src" = none ]; then
      # The env token was rejected and nothing below it yielded a bearer: say
      # so, with the mint's outcome, rather than blaming the 401 on a
      # credential that was never sent.
      _mn="$(cat "$TMP/mint.note" 2>/dev/null)"
      COORD_FAILED=1; probe coord failed "coord answered HTTP 401 to \$COORD_DEVICE_JWT, and no usable device JWT stood below it (~/.qontinui/coord-device-jwt, bootstrap mint all missed${_mn:+; mint: $_mn})"
    elif [ "$code" != 200 ]; then
      COORD_FAILED=1; probe coord failed "GET /pr-merge/prs?include_merged=$hours answered HTTP ${code:-000} (credential: $src) $(head -1 "$TMP/coord.err" 2>/dev/null | cut -c1-120)"
    else
      nums="${PR_NUM[*]+${PR_NUM[*]}}"
      if "$PY" -c '
import json, sys
repo, branch, nums = sys.argv[1], sys.argv[2], set(sys.argv[3].split())
d = json.load(sys.stdin)
rows = d.get("prs") if isinstance(d, dict) else d
if not isinstance(rows, list): sys.exit(3)
for r in rows:
    if not isinstance(r, dict) or r.get("repo") != repo: continue
    if r.get("branch") != branch and str(r.get("pr_number")) not in nums: continue
    print("\t".join(str(r.get(k) or "") for k in ("pr_number", "merge_commit_sha", "close_cause", "pr_state", "merged_at")))
' "$SLUG" "$BRANCH" "$nums" < "$TMP/coord.json" > "$TMP/coord.tsv" 2>/dev/null; then
        _d=""; _k=0
        while IFS=$'\x1f' read -r n mc cc ps ma; do
          [ -n "$n" ] || continue; _k=$((_k + 1)); _d="$_d #$n $ps${mc:+ merge_commit ${mc:0:12}}${cc:+ close_cause $cc};"
          [ -n "$mc" ] || continue
          attach_pr_candidate coord_land_stamp "$n" "$mc" "$(pr_base "$n")" "coord pr_state=$ps close_cause=${cc:-null} merged_at=$ma"
        done < <(tr -d '\r' < "$TMP/coord.tsv" | tr '\t' '\037')   # Windows Python prints CRLF
        if [ "$clamped" = 1 ]; then
          COORD_FAILED=1; probe coord incomplete "include_merged window clamped to ${hours}h, younger than the oldest commit; $_k row(s):${_d:- none}"
        else
          probe coord ok "include_merged=${hours}h (credential: $src): $_k row(s) for $BRANCH:${_d:- none}"
        fi
      else
        COORD_FAILED=1; probe coord failed "the coord listing did not parse as {prs:[...]}"
      fi
    fi
  fi
  compute_strength
fi

if [ "$STRENGTH" != PROVEN_LANDED ] && { [ "$GH_FAILED" = 1 ] || [ "$COORD_FAILED" = 1 ] || [ "$OBJ_MISSING" = 1 ] \
     || [ "$PAIR_FAILED" = 1 ] || [ "$UP_STALE" = 1 ]; }; then
  FLOOR="$STRENGTH"; STRENGTH=UNKNOWN
fi

# ---------------------------------------------------------------------------
# door 4: supersession (SUPERSESSION in the header). Reports on every commit the
# land doors left at NONE; DECIDES only from a clean NONE.

# The classified lifecycle vocabulary of served policy `plan-discipline`, for
# --corpus plan-document. Anything outside it is unparseable, and an unparseable
# status is UNKNOWN -- never "later". `/verify-plan-status`'s PARTIAL and
# NOT STARTED are deliberately absent: they describe implementation progress,
# not the attested lifecycle, and ranking them would invent an ordering policy
# does not define.
plan_status_rank() { # <status text> -> rank, or 1 when unparseable
  case "$(printf '%s' "$1" | tr 'A-Z' 'a-z' | tr -s ' ' | sed 's/^ *//; s/ *$//')" in
    draft) printf 1 ;;
    vetted) printf 2 ;;
    'in progress'|in_progress) printf 3 ;;
    shipped) printf 4 ;;
    superseded|obsolete) printf 5 ;;
    *) return 1 ;;
  esac
}
# The status word of the leading status blockquote, i.e. the `> **Status:` line
# in the `>` block before the first `## ` heading. Anything after the first
# comma, period or `**` is prose, and a trailing date is not the word.
plan_status_of() { # <file> -> the status word, or 1
  awk '
    /^## / { exit }
    /^> \*\*Status:/ {
      s = $0
      sub(/^> \*\*Status:[[:space:]]*/, "", s)
      sub(/\*\*.*$/, "", s); sub(/[,.].*$/, "", s)
      sub(/[[:space:]]+[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9].*$/, "", s)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", s)
      if (s != "") { print s; found = 1 }
      exit
    }
    END { exit !found }
  ' "$1" 2>/dev/null
}
# "<first> <last>" of the leading, STRICTLY CONTIGUOUS `>` block before the
# first `## ` heading -- the bounds branch-only lines must lie inside.
plan_status_bounds() { # <file> -> "<start> <end>", or 1
  awk '
    /^## / { exit }
    /^>/ { if (!s) { s = NR; e = NR } else if (NR == e + 1) { e = NR } else exit; next }
    { if (s) exit }
    END { if (s) { print s, e } else { exit 1 } }
  ' "$1" 2>/dev/null
}
# The lines C ADDS to <path>, as "<line_no in C's post-image>\t<text>".
# The `+++ `/`--- ` file headers are skipped by POSITION (anything before the
# first `@@` of a file), never by CONTENT. A content line `++ foo` is printed by
# `git diff` as `+++ foo`, so a `/^\+\+\+ /` filter DROPS it: the line never
# enters the added set, the commit reads "adds nothing upstream lacks", and the
# `branch_only` list the adjudicator is told to quote comes back EMPTY while
# corroborating a false SUPERSEDED. It also desynchronised `n` for every later
# added line in the hunk. The sibling primitive in the multiset test already
# gates on hunk state; this one now matches it.
added_with_offsets() { # <par> <l> <path> -> 0, or 1 when git itself failed
  g diff --no-renames --no-color --no-ext-diff --no-textconv -U0 "$1" "$2" \
     -- ":(top,literal)$3" > "$TMP/s.rawdiff" 2>/dev/null || return 1
  awk '
        /^diff --git / { n = 0; h = 0; next }
        /^@@/ { if (match($0, /\+[0-9]+/)) n = substr($0, RSTART + 1, RLENGTH - 1) + 0; h = 1; next }
        !h { next }
        /^\+/ { printf "%d\t%s\n", n, substr($0, 2); n++; next }
      ' "$TMP/s.rawdiff"
}
# Did an upstream commit LATER than <c_epoch> REMOVE this exact line from
# <path>? That is an observed upstream DECISION. `git log -S` finds every
# commit where the line's count CHANGED; the count comparison is what tells a
# removal from an addition. A failed read returns 2 (UNKNOWN), never 1 (no).
upstream_removed_later() { # <path> <line> <c_epoch> -> 0 yes, 1 no, 2 unknown
  local path="$1" line="$2" since="$3" h ct before after
  [ -n "$line" ] || return 1          # a blank line is no evidence of anything
  g log "$UPSTREAM" --format='%H%x1f%ct' -S"$line" --max-count=25 -- ":(top,literal)$path" > "$TMP/s.pick" 2>/dev/null || return 2
  while IFS=$'\x1f' read -r h ct; do
    [ -n "$h" ] || continue
    case "$ct" in ''|*[!0-9]*) continue ;; esac
    [ "$ct" -gt "$since" ] || continue
    # `|| true` on both reads used to swallow git's status, so an UNREADABLE
    # object at $h (not merely an absent path) gave after=0 against a non-zero
    # before -- a failed read manufacturing the affirmative, which is the exact
    # shape D6 forbids. Resolve presence first, then read the git stage's own
    # status out of PIPESTATUS, and answer 2 (UNKNOWN) rather than 0.
    if g cat-file -e "$h^:$path" 2>/dev/null; then
      g show "$h^:$path" > "$TMP/s.rmbefore" 2>/dev/null || return 2
      before="$(grep -cFx -- "$line" "$TMP/s.rmbefore" || true)"
    else before=0; fi
    if g cat-file -e "$h:$path" 2>/dev/null; then
      g show "$h:$path" > "$TMP/s.rmafter" 2>/dev/null || return 2
      after="$(grep -cFx -- "$line" "$TMP/s.rmafter" || true)"
    else after=0; fi
    case "${before}${after}" in *[!0-9]*) return 2 ;; esac
    [ "$after" -lt "$before" ] && return 0
  done < "$TMP/s.pick"
  return 1
}

supersede_commit() { # <ci> -> sets S_VERDICT[ci] and S_JSON[ci]
  local ci="$1" l par rec a d path
  # Every temporary is local: this function runs once per NONE commit and the
  # plan-document arm's values (_bs/_be in particular) are quoted into a reason
  # string, so a leak across commits would attribute one commit's line numbers
  # to another's report.
  local _pd_reason _bounds _bs _be _outside _bst _ust _br _ur _urc
  local verdict=SUPERSEDED reason="" pjson="" pfirst=1
  local up_exists up_newest up_at up_n br_n pverdict preason bo_json bofirst off txt disp n_bo
  local _ep _el _eu _rec _cum_bo=0 _pd_settled _pd_carry="" _ubounds _awo_rc _bodisp
  l="${C_SHA[$ci]}"
  # max(%at, %ct), and an unreadable date is UNKNOWN. `${...:-0}` would make
  # EVERY upstream commit "later than" this one, turning a failed read into the
  # affirmative -- the precise shape D6 forbids.
  local cdate _at="${C_DATE[$ci]:-}" _ct="${C_CT[$ci]:-}"
  case "${_at}${_ct}" in *[!0-9]*|'') _at=""; _ct="" ;; esac
  if [ -n "$_at" ] && [ -n "$_ct" ]; then
    cdate="$_at"; [ "$_ct" -gt "$_at" ] && cdate="$_ct"
  else
    S_VERDICT[$ci]=UNKNOWN; SUPER_FAILED=1
    S_JSON[$ci]="{\"verdict\":\"UNKNOWN\",\"corpus\":$(jstr "$CORPUS"),\"reason\":$(jstr "this commit's own author/committer date could not be read, so \"later than this commit\" is undecidable"),\"paths\":[]}"
    return 0
  fi
  par="$(g rev-parse -q --verify "$l^1" 2>/dev/null)" || par="$EMPTY_TREE"
  if ! g diff --no-renames --ignore-submodules=none --numstat -z "$par" "$l" > "$TMP/s.numstat" 2>/dev/null; then
    S_VERDICT[$ci]=UNKNOWN; SUPER_FAILED=1
    S_JSON[$ci]="{\"verdict\":\"UNKNOWN\",\"corpus\":$(jstr "$CORPUS"),\"reason\":$(jstr "the commit's own diff could not be read"),\"paths\":[]}"
    return 0
  fi
  while IFS= read -r -d '' rec; do
    a="${rec%%$'\t'*}"; rec="${rec#*$'\t'}"; d="${rec%%$'\t'*}"; path="${rec#*$'\t'}"
    pverdict=SUPERSEDED; preason=""; bo_json=""; bofirst=1; n_bo=0
    # The TREE ENTRIES first -- "<mode> <oid>", "" when the path is absent -- at
    # C's parent, at C, and at the upstream tip. Every guard below is a statement
    # about them, and an unreadable one is UNKNOWN rather than a missing guard.
    if ! _ep="$(entry_of "$par" "$path")" || ! _el="$(entry_of "$l" "$path")" || ! _eu="$(entry_of "$UPSTREAM" "$path")"; then
      pverdict=UNKNOWN; SUPER_FAILED=1
      preason="the tree entry for '$path' could not be read on one of C^, C or $UPSTREAM"
    fi
    # A GITLINK (submodule pointer) has no lines at all. `--numstat` prints the
    # `-`/`-` sentinel for BINARY blobs ONLY -- a gitlink reports `1 1` -- so the
    # binary test below does NOT cover it, and a comment claiming otherwise
    # guarded nothing. Test the MODE, exactly as the land-candidate path does.
    if [ "$pverdict" = SUPERSEDED ] && { [ "${_el%% *}" = 160000 ] || [ "${_ep%% *}" = 160000 ]; }; then
      pverdict=SUPERSEDED_CANDIDATE
      preason="a gitlink (submodule pointer) carries no lines an upstream removal could be observed on"
    fi
    if [ "$pverdict" = SUPERSEDED ] && { [ "$a" = "-" ] || [ "$d" = "-" ]; }; then
      pverdict=SUPERSEDED_CANDIDATE; preason="a binary change carries no lines an upstream removal could be observed on"
    fi
    # THE MODE IS CONTENT, and no added-line test observes it. The guard below
    # compares C^ -> C; this one compares C -> UPSTREAM, which is the question
    # that actually matters: upstream can hold every line C added at a DIFFERENT
    # mode, and the exec bit is then the branch's only contribution, unlanded.
    # Unconditional -- a mode change is never a plan status stamp, so the
    # plan-document profile does not exempt it.
    if [ "$pverdict" = SUPERSEDED ] && [ -n "$_el" ] && [ -n "$_eu" ] \
       && [ "${_el%% *}" != "${_eu%% *}" ]; then
      pverdict=SUPERSEDED_CANDIDATE
      preason="the upstream copy of '$path' is mode ${_eu%% *} where this commit leaves it ${_el%% *}; a mode is content no added-line test observes"
    fi
    if g cat-file -e "$UPSTREAM:$path" 2>/dev/null; then up_exists=true; else up_exists=false; fi
    up_newest="$(g log -1 --format=%H "$UPSTREAM" -- ":(top,literal)$path" 2>/dev/null)"
    up_at="$(g log -1 --format=%cI "$UPSTREAM" -- ":(top,literal)$path" 2>/dev/null)"
    blob_or_empty "$UPSTREAM:$path" "$TMP/s.up" || :
    blob_or_empty "$l:$path" "$TMP/s.post" || :
    up_n="$(awk 'END{print NR+0}' "$TMP/s.up")"; br_n="$(awk 'END{print NR+0}' "$TMP/s.post")"
    # A FAIL-CLOSED guard on an error path, kept deliberately although no fixture
    # can reach it any more: the one route that DID reach it -- a textconv driver
    # whose binary is absent, which killed `git diff -U0` while `--numstat`
    # succeeded -- is now closed at the source by `--no-textconv`, so it is not
    # given a mutant it could never discharge.
    # `$?` of the FUNCTION, never PIPESTATUS. After a simple command (a function
    # call is one) bash resets PIPESTATUS to a single element holding that
    # command's status, so reading PIPESTATUS[0] here saw awk's success and the
    # control was DEAD -- deleting it left the suite green. A failed `git diff`
    # yields no added lines, hence n_bo=0, hence the affirmative arm, so this is
    # a failed read becoming SUPERSEDED if it is not caught.
    added_with_offsets "$par" "$l" "$path" > "$TMP/s.added" 2>/dev/null; _awo_rc=$?
    if [ "$_awo_rc" -ne 0 ] && [ "$pverdict" = SUPERSEDED ]; then
      pverdict=UNKNOWN; SUPER_FAILED=1
      preason="the diff of '$path' could not be read, so its added lines are unknown"
    fi
    # BRANCH-ONLY: an added copy the upstream tip does not account for, counted
    # as a MULTISET so a duplicate or a pre-existing line cannot stand in for it.
    awk '
      FILENAME == ARGV[1] { up[$0]++; next }
      FILENAME == ARGV[2] { post[$0]++; next }
      {
        off = $0; sub(/\t.*/, "", off)
        line = $0; sub(/^[0-9]*\t/, "", line)
        def = post[line] - up[line]; if (def < 0) def = 0
        if (++taken[line] <= def) printf "%s\t%s\n", off, line
      }
    ' "$TMP/s.up" "$TMP/s.post" "$TMP/s.added" > "$TMP/s.bo" 2>/dev/null || : > "$TMP/s.bo"
    n_bo="$(awk 'END{print NR+0}' "$TMP/s.bo")"
    if [ "$pverdict" = SUPERSEDED ] && [ "$up_exists" != true ]; then
      pverdict=SUPERSEDED_CANDIDATE; preason="the path is absent from $UPSTREAM, so upstream never carried it forward"
    fi
    # CUMULATIVE across the commit, which is what the header documents and what
    # actually bounds the work: the general arm runs a `git log -S` history walk
    # per branch-only line, so a commit touching 40 paths at just under a
    # per-path cap would run thousands of walks inside the nightly sweep.
    _cum_bo=$((_cum_bo + n_bo))
    if [ "$pverdict" = SUPERSEDED ] && [ "$n_bo" -gt 0 ] && [ "$_cum_bo" -gt "$MAX_SUPER_LINES" ]; then
      pverdict=SUPERSEDED_CANDIDATE; preason="this commit's branch-only lines reach $_cum_bo, above LAND_EVIDENCE_SUPERSEDE_MAX_LINES ($MAX_SUPER_LINES); the general arm did not probe '$path'"
    fi
    # --corpus plan-document (D4). The profile is tried FIRST and the general arm
    # is a genuine FALLBACK: a commit touching both plans/x.md and src/y.py gets
    # the profile's answer for the first and the general arm's for the second,
    # rather than an automatic SUPERSEDED_CANDIDATE for everything the profile
    # does not recognise.
    _pd_settled=0
    if [ "$pverdict" = SUPERSEDED ] && [ "$CORPUS" = plan-document ]; then
      _pd_reason=""
      case "$path" in
        plans/*.md) ;;
        *) _pd_reason="'$path' is not plans/*.md" ;;
      esac
      if [ -z "$_pd_reason" ]; then
        # The confinement test compares the two files WHOLE with their leading
        # status blockquotes removed. An earlier draft only checked that each
        # branch-only ADDED line fell inside the block, which said nothing about
        # lines the commit REMOVED from the body -- a plan edit that quietly
        # dropped a section would have passed. If the remainders are byte-equal,
        # then EVERY difference in either direction is inside the status block.
        _bounds="$(plan_status_bounds "$TMP/s.post")" || _bounds=""
        _ubounds="$(plan_status_bounds "$TMP/s.up")" || _ubounds=""
        if [ -z "$_bounds" ] || [ -z "$_ubounds" ]; then
          _pd_reason="a leading status blockquote is missing on $([ -z "$_bounds" ] && printf 'the branch'; [ -z "$_bounds" ] && [ -z "$_ubounds" ] && printf ' and '; [ -z "$_ubounds" ] && printf 'the upstream') side, so the change cannot be confined to one"
        else
          _bs="${_bounds%% *}"; _be="${_bounds##* }"
          sed "${_bs},${_be}d" "$TMP/s.post" > "$TMP/s.post.body" 2>/dev/null || : > "$TMP/s.post.body"
          sed "${_ubounds%% *},${_ubounds##* }d" "$TMP/s.up" > "$TMP/s.up.body" 2>/dev/null || : > "$TMP/s.up.body"
          # An EMPTY body on either side proves nothing: a plan whose every line
          # is inside the leading `>` run (no `## ` heading at all) deletes to
          # nothing on both sides, `cmp` succeeds vacuously, and a branch holding
          # unique design lines against a one-line upstream would exit 5 on an
          # empty-vs-empty comparison. Refuse it.
          if [ ! -s "$TMP/s.post.body" ] || [ ! -s "$TMP/s.up.body" ]; then
            _pd_reason="one of the two copies has NO body outside its leading status blockquote, so an identical-bodies comparison would be vacuous"
          # A status blockquote is a stamp, not a chapter. An unbounded leading
          # `>` run lets substance hide inside the very region the profile is
          # licensed to ignore, so cap it.
          elif [ "$((_be - _bs + 1))" -gt "$MAX_STATUS_BLOCK_LINES" ] \
            || [ "$(( ${_ubounds##* } - ${_ubounds%% *} + 1 ))" -gt "$MAX_STATUS_BLOCK_LINES" ]; then
            _pd_reason="a leading status blockquote longer than $MAX_STATUS_BLOCK_LINES lines is not a status stamp; substance inside it would be ignored"
          elif ! cmp -s "$TMP/s.post.body" "$TMP/s.up.body"; then
            _pd_reason="the two copies differ OUTSIDE the leading status blockquote: that is substance, not a superseded status stamp"
          fi
        fi
      fi
      if [ -z "$_pd_reason" ]; then
        _bst="$(plan_status_of "$TMP/s.post")" || _bst=""
        _ust="$(plan_status_of "$TMP/s.up")" || _ust=""
        _br="$(plan_status_rank "$_bst")" || _br=""
        _ur="$(plan_status_rank "$_ust")" || _ur=""
        if [ -z "$_br" ] || [ -z "$_ur" ]; then
          _pd_reason="status unparseable on $([ -z "$_br" ] && printf 'the branch'; [ -z "$_br" ] && [ -z "$_ur" ] && printf ' and '; [ -z "$_ur" ] && printf 'upstream') side (branch '${_bst:-<none>}', upstream '${_ust:-<none>}'): UNKNOWN is never 'later'"
        elif [ "$_ur" -le "$_br" ]; then
          _pd_reason="upstream status '$_ust' is not STRICTLY later than the branch's '$_bst'"
        else
          preason="plan-document: the two copies are identical outside the leading status blockquote (branch lines $_bs-$_be) and upstream is '$_ust' against the branch's '$_bst'"
          _pd_settled=1
        fi
      fi
      # NOT settled by the profile -> fall through to the general arm, carrying
      # the profile's reason only if the general arm cannot settle it either.
      [ "$_pd_settled" = 1 ] || _pd_carry="$_pd_reason"
    fi
    # THE REMOVAL HOLE. This arm observes ADDED lines only: for each added line it
    # asks whether upstream later removed it. A commit that REMOVES content or
    # DELETES a path makes a claim in the OPPOSITE direction -- "upstream should
    # no longer hold this" -- and no added-line test can establish an upstream
    # decision about it. Without this, such a commit reaches `n_bo == 0` and is
    # reported SUPERSEDED with the reason "adds no line this path's upstream copy
    # does not already account for": a deletion that never landed, declared safe
    # to discard while upstream still holds the content.
    #
    # IT IS GATED ON `_pd_settled`, NOT ON `$CORPUS`, AND IT SITS AFTER THE
    # PROFILE FOR THAT REASON. The plan-document profile is exempt because its
    # own test compares the two files WHOLE, so a removal inside the status block
    # is covered there -- but that exemption is only earned for a path the
    # profile actually SETTLED. Keyed on `$CORPUS` and placed before the profile,
    # it switched the guard off for the whole RUN, including every path the
    # profile never settles: non-plan paths, and `plans/*.md` where the profile
    # bailed. A branch deleting a stale plan then exited 5 with the reason "adds
    # no line ... and it removes nothing" -- for a commit whose entire content is
    # a deletion, on the one corpus the profile is advertised for.
    if [ "$pverdict" = SUPERSEDED ] && [ "$_pd_settled" != 1 ]; then
      if [ -z "$_el" ]; then
        pverdict=SUPERSEDED_CANDIDATE
        preason="this commit DELETES '$path'; supersession observes ADDED lines only, so no upstream decision about a deletion can be established"
      elif [ "${d:-0}" != 0 ]; then
        pverdict=SUPERSEDED_CANDIDATE
        preason="this commit REMOVES ${d} line(s) from '$path'; supersession observes ADDED lines only, so no upstream decision about a removal can be established"
      elif [ -n "$_ep" ] && [ "${_el%% *}" != "${_ep%% *}" ]; then
        pverdict=SUPERSEDED_CANDIDATE
        preason="this commit changes the MODE of '$path' (${_ep%% *} -> ${_el%% *}); supersession observes ADDED lines only, so no upstream decision about a mode change can be established"
      fi
    fi
    if [ "$pverdict" = SUPERSEDED ] && [ "$_pd_settled" != 1 ]; then
      # The general arm (D2): every branch-only line must have been REMOVED from
      # this path by an upstream commit LATER than C.
      if [ "$n_bo" -eq 0 ]; then
        # C added nothing this path's upstream copy does not hold. Every arm that
        # could make that unsafe -- a removal, a deletion, a mode change, a
        # gitlink, a binary blob, an unreadable diff -- has already been refused
        # above, so what remains really is "upstream holds everything C added".
        preason="the commit adds no line this path's upstream copy does not already account for, and it removes nothing"
      else
        while IFS= read -r _rec; do
          # NOT `IFS=$'\t' read -r off txt`: tab is IFS WHITESPACE, so runs of it
          # collapse and a LEADING tab is stripped entirely. That silently
          # rewrote every tab-indented line -- making `grep -cFx` match a
          # different line (a false removed_later_upstream) and falsifying the
          # `text` field SKILL.md Step 3e(v) tells the adjudicator to QUOTE
          # rather than re-derive. Two parameter expansions keep the byte exact.
          off="${_rec%%$'\t'*}"; txt="${_rec#*$'\t'}"
          upstream_removed_later "$path" "$txt" "$cdate"; _urc=$?
          case "$_urc" in
            0) disp=removed_later_upstream ;;
            2) disp=unknown; pverdict=UNKNOWN; SUPER_FAILED=1
               preason="the upstream history of '$path' could not be read; a failed door never manufactures supersession" ;;
            # Two different worlds reach this arm and NEITHER is decidable: the
            # line was never upstream at all, or upstream removed it BEFORE this
            # commit was written. Both mean no upstream DECISION about this
            # commit's content can be observed, so the wording must not claim
            # the first one -- it cannot tell them apart.
            *) disp=no_later_upstream_removal
               [ "$pverdict" = SUPERSEDED ] && { pverdict=SUPERSEDED_CANDIDATE
                 preason="no upstream commit LATER than this one removed the branch-only line at $path:$off (it was never upstream, or upstream removed it before this commit was written): no upstream DECISION about this content can be observed"; } ;;
          esac
          [ "$bofirst" = 1 ] || bo_json="$bo_json,"; bofirst=0
          bo_json="$bo_json{\"line_no\":$(jnum "$off"),\"text\":$(jstr "$txt"),\"disposition\":$(jstr "$disp")}"
        done < "$TMP/s.bo"
        [ "$pverdict" = SUPERSEDED ] && preason="every branch-only line was removed from '$path' by a LATER upstream commit"
      fi
      # The profile looked and could not settle it; say so beside whatever the
      # general arm concluded, so a reader is not left wondering why the profile
      # they asked for is silent on this path.
      if [ -n "${_pd_carry:-}" ] && [ "$pverdict" != SUPERSEDED ]; then
        preason="$preason (--corpus plan-document did not apply: ${_pd_carry})"
      fi
      _pd_carry=""
    fi
    _pd_carry=""
    # The report carries the branch-only lines whether or not the arm probed
    # them -- that list IS the D3 report, and its absence is what made the
    # 2026-09-16 session rebuild the answer by hand from git primitives.
    if [ "$bofirst" = 1 ]; then
      bo_json=""; bofirst=1
      _bodisp=unprobed
      [ "$_pd_settled" = 1 ] && _bodisp=inside_status_block
      while IFS= read -r _rec; do   # see the tab note above; never IFS=$'\t'
        off="${_rec%%$'\t'*}"; txt="${_rec#*$'\t'}"
        [ "$bofirst" = 1 ] || bo_json="$bo_json,"; bofirst=0
        bo_json="$bo_json{\"line_no\":$(jnum "$off"),\"text\":$(jstr "$txt"),\"disposition\":$(jstr "$_bodisp")}"
      done < "$TMP/s.bo"
    fi
    [ "$pfirst" = 1 ] || pjson="$pjson,"; pfirst=0
    pjson="$pjson{\"path\":$(jstr "$path"),\"upstream_exists\":$up_exists,\"upstream_newest\":$(jstr "$up_newest"),\"upstream_newest_at\":$(jstr "$up_at"),\"upstream_lines\":$(jnum "$up_n"),\"branch_lines\":$(jnum "$br_n"),\"branch_only\":[$bo_json],\"verdict\":$(jstr "$pverdict"),\"reason\":$(jstr "$preason")}"
    # A commit is only as strong as its weakest path.
    case "$pverdict" in
      UNKNOWN) verdict=UNKNOWN; reason="$path: $preason" ;;
      SUPERSEDED_CANDIDATE) [ "$verdict" = UNKNOWN ] || { verdict=SUPERSEDED_CANDIDATE; reason="$path: $preason"; } ;;
      *) [ -n "$reason" ] || reason="$path: $preason" ;;
    esac
  done < "$TMP/s.numstat"
  if [ "$pfirst" = 1 ]; then
    verdict=SUPERSEDED_CANDIDATE
    reason="this commit touches no path (its tree equals its parent's), so there is no upstream decision supersession could observe; the verdict came from no examination at all"
  fi
  S_VERDICT[$ci]="$verdict"
  S_JSON[$ci]="{\"verdict\":$(jstr "$verdict"),\"corpus\":$(jstr "$CORPUS"),\"reason\":$(jstr "$reason"),\"paths\":[$pjson]}"
}

_sup_n=0; _sup_sup=0; _sup_cand=0; _sup_unk=0
for i in "${!C_SHA[@]}"; do
  [ "${C_VERDICT[$i]:-NONE}" = NONE ] || continue
  _sup_n=$((_sup_n + 1)); supersede_commit "$i"
  case "${S_VERDICT[$i]}" in
    SUPERSEDED) _sup_sup=$((_sup_sup + 1)) ;;
    SUPERSEDED_CANDIDATE) _sup_cand=$((_sup_cand + 1)) ;;
    *) _sup_unk=$((_sup_unk + 1)) ;;
  esac
done
if [ "$_sup_n" -eq 0 ]; then
  probe supersession skipped "no commit is at NONE, so there is nothing supersession could add"
else
  probe supersession "$([ "$SUPER_FAILED" = 1 ] && echo failed || echo ok)" \
    "$_sup_n NONE commit(s) probed${CORPUS:+ under --corpus $CORPUS}: $_sup_sup SUPERSEDED, $_sup_cand SUPERSEDED_CANDIDATE, $_sup_unk UNKNOWN"
fi
# DECIDE only from a CLEAN NONE. `STRENGTH = NONE` is the whole of that rule and
# carries the door-failure half on its own: the block above has already turned
# any failed land door into UNKNOWN, so a failed door can never be at NONE here.
# An earlier draft tested a separate LAND_DOOR_FAILED flag beside it; the
# mutation harness proved that flag DEAD -- removing it reddened nothing -- which
# is exactly the unpinned-control shape this plan exists to object to, so it is
# gone rather than left as decoration. The branch is SUPERSEDED only when EVERY
# commit is: the all-or-nothing rule PROVEN_LANDED already uses.
if [ "$STRENGTH" = NONE ] && [ "$SUPER_FAILED" = 0 ] \
   && [ "$_sup_n" -gt 0 ] && [ "$_sup_sup" = "$_sup_n" ]; then
  STRENGTH=SUPERSEDED
  for i in "${!C_SHA[@]}"; do [ "${C_VERDICT[$i]:-NONE}" = NONE ] && C_VERDICT[$i]=SUPERSEDED; done
elif [ "$SUPER_FAILED" = 1 ] && [ "$STRENGTH" = NONE ]; then
  FLOOR="$STRENGTH"; STRENGTH=UNKNOWN
fi
emit
