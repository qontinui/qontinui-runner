#!/usr/bin/env bash
# pinned-read.sh — the fleet's ONE pinned read: FOUND / MISSING_AT_REF / UNKNOWN.
#
# SOURCE this for the functions, or EXECUTE it for the four verbs. Both modes
# run the same code; there is deliberately no second implementation, and no
# Python twin (see "ONE PRODUCER" below).
#
# ── WHY THIS EXISTS ─────────────────────────────────────────────────────────
#
# The fleet prescribes `git show <ref>:<path>` as the safe "read what actually
# landed" idiom, at the exact moment an agent is about to make a NEGATIVE claim
# from a stale working tree. That idiom cannot carry the answer it is being
# asked for. Measured on this box (git 2.47.3, Linux, no MSYS involved):
#
#   git show <ref>:<present-file>    -> rc 0, content
#   git show <ref>:<missing-path>    -> rc 128, EMPTY stdout
#   git show <ref>:<a-directory>/    -> rc 0, a TREE LISTING on stdout
#
# The 128 is the only thing separating "this file says nothing about X" from
# "this file is not in this ref". It is destroyed the moment the read is piped —
# which is how it is always used:
#
#   git show <ref>:<path> 2>/dev/null | grep -c <pat>
#     pattern absent from a PRESENT file  ->  `0`, rc 1
#     path absent from the REF            ->  `0`, rc 1     <-- identical
#
# `set -o pipefail` does NOT rescue it: both arms still exit 1, because
# `grep -c` legitimately exits 1 on zero matches. There is no shell-level fix —
# the exit code has already been spent. This matters MOST in the situation the
# idiom is prescribed for: a path is likeliest to be absent from `origin/main`
# exactly when a file was renamed or moved, which is the common case when
# checking a stale tree against main. The remedy fails hardest at its own use
# case [policy: verification-and-evidence `silent-empty-is-unknown`].
#
# ── ⚠️ THE OBVIOUS SUBSTITUTE HAS THE SAME DEFECT ───────────────────────────
#
# `git grep <pat> <ref> -- <pathspec>` is mangle-immune and prefixes its own
# ref, so it is the natural replacement and IS already prescribed in one place
# on this fleet (.claude/commands/merge-train-steward.md). It trades one silent
# absence for another. Measured:
#
#   git grep -q <pat> <ref> -- ':(glob)ok/**'        -> 0 (match)
#   git grep -q <nope> <ref> -- ':(glob)ok/**'       -> 1 (no match)
#   git grep -q <pat> <ref> -- ':(glob)nonexistent/**'
#                                                    -> 1, ZERO stderr, no warning
#
# `git grep` has NO exit code distinguishing "the pathspec matched no files at
# that ref" from "the pattern is not there". So the `grep` verb here probes the
# pathspec FIRST (`git grep -l '' <sha> -- <pathspec>`) and reports an empty
# pathspec as its own state 3, never as "no match".
#
# ── ⚠️ THE ERROR IS NOT PURELY DIRECTIONAL — TWO FALSE-POSITIVE PATHS ───────
#
# It is often said that a stale read can only make something look ABSENT, never
# present. That is true of the TREE. It is false of these reads, twice:
#
#   (a) A DIRECTORY reads as content. `git show <ref>:<dir>/` exits 0 and prints
#       a tree listing; piped to grep, FILENAMES are counted as if they were
#       file content. A trailing-slash typo turns a false absence into a false
#       PRESENCE. This is why the existence oracle here is `cat-file -t` == blob
#       and NOT `cat-file -e`: measured, `cat-file -e <ref>:<a-directory>`
#       returns 0. An oracle with a known hole is not an oracle.
#
#   (b) `git ls-files --with-tree=<ref>` LEAKS THE INDEX. It unions the index
#       with the tree, so a locally staged file is reported as being on the ref.
#       That is strictly worse than a stale grep: it manufactures evidence FOR a
#       claim about what shipped. Nothing here uses it.
#
# ⚠️ Do NOT "harmonize" the oracle here with scripts/lint-doc-paths.py, which
# deliberately chose `ls-tree` OVER `cat-file -e` "because it answers for
# directories too, and several pointers here name one." The two answer different
# questions: that linter validates a POINTER, for which a directory is a
# legitimate target; this helper is about to READ A FILE, for which a directory
# is not. Same repo, opposite requirement, both correct.
#
# ── ⚠️ THE LIVE COLLISION: A MISSING PATH HASHES TO A WELL-FORMED DIGEST ────
#
# Two shipped surfaces pipe a pinned read straight into a hash and compare it.
# Measured:
#
#   git show <ref>:<missing> | git hash-object --stdin -> e69de29bb2d1d643...
#   git show <ref>:<missing> | sha256sum               -> e3b0c44298fc1c14...
#
# Those are the digests of NOTHING. They look like measurements, and two
# DIFFERENT absent paths compare EQUAL — so two missing files verify as
# identical. This is the same sentinel-collision defect
# scripts/detector_reach/__init__.py documents for `tree:`
# (qontinui-claude-config#778, where cargo-guard's `unknown` key collided with
# itself and served a cache HIT), reappearing on a surface nothing checks. The
# `cat` verb here writes content to stdout ONLY in state 0, so a caller that
# pipes and ignores the exit code gets NOTHING rather than something wrong.
#
# ── THE GRAMMAR ─────────────────────────────────────────────────────────────
#
#   pin: ref=<as-given> sha=<40hex|UNKNOWN> path=<path> \
#        state=<FOUND|MISSING_AT_REF|PATHSPEC_EMPTY|UNKNOWN> \
#        type=<blob|tree|none|UNKNOWN> measured=<ISO time>
#
# Owned by `PIN_LINE_FORMAT` / `PIN_LINE_RE` / `parse_pin_line` in
# scripts/detector_reach/__init__.py, beside `REACH_LINE_FORMAT`,
# `CENSUS_TRAILER_FORMAT` and `TREE_IDENTITY_FORMAT`. This file does not import
# that one (bash cannot); scripts/pinned-read-test.sh asserts that every line
# this file prints parses under `PIN_LINE_RE`, which is what keeps the two from
# drifting — the same arrangement reach-grep.sh and tree-identity.sh have.
#
# Every field has an UNKNOWN arm that parses, for the reason `routes=UNKNOWN`
# and `dirty=unknown` do: a probe that could not measure emits a LINE, so "I did
# not look" is visible rather than absent.
#
# ⚠️ `UNKNOWN` here is a SENTINEL, NOT A VALUE. Two unresolved reads carry the
# same token and would compare EQUAL under `==` — the #778 collision again, and
# the empty-blob digest above is a live instance of exactly it. Any consumer
# comparing two `pin:` lines must treat "either side unresolved" as UNKNOWN and
# NEVER as SAME.
#
# ⚠️ `path=` is PERCENT-ENCODED for whitespace and `%` (`%20`, `%09`, `%25`) so
# that every field stays a single whitespace-free token, as in the other three
# grammars. `parse_pin_line` decodes it. A path is never emitted raw with a
# space in it, because that would silently reshape the line's field count.
#
# ── THE REF IS RESOLVED TO A SHA EXACTLY ONCE, AND EMITTED ──────────────────
#
# `git rev-parse --verify -q <ref>^{commit}` once per invocation; every
# subsequent git call uses the SHA. Three things fall out:
#
#   1. Naming and reading cannot diverge. A report that says "measured against
#      origin/main" and a read that went through origin/main are the same fact
#      MECHANICALLY, rather than by the reader's discipline.
#   2. A 40-hex sha contains no slash, which removes the MSYS/Git-Bash ref
#      mangle outright (that mangle needs a slash in the ref AND a leading dot
#      in the path).
#   3. It closes the ref-churn hole. `origin/main` moves under a long session —
#      measured twice minutes apart while this helper was being written, and it
#      had MOVED. Two reads that both truthfully say "against origin/main" then
#      read two DIFFERENT trees. The dossier's exit metric ("names a ref AND was
#      read through it") is not satisfiable by a branch name at all.
#
# A bogus ref exits 128 at `rev-parse`, which is rendered UNKNOWN (2), never
# MISSING. "I could not resolve the ref" is not "the file is not there".
#
# ── ONE PRODUCER, NO TWIN ───────────────────────────────────────────────────
#
# Consumers are bash hooks and a Python linter that only needs to PARSE the
# line, not produce it — so this follows lib/tree-identity.sh (bash-only,
# grammar registered in Python) rather than reach-grep.sh (which has a Python
# twin). A second producer would be a second, blinder implementation.
#
# ── READ-ONLY BY CONSTRUCTION ───────────────────────────────────────────────
#
# No `stash create`, no ref write, no fetch, no network. The helper must never
# mutate the tree it is measuring, and must never make the network call that
# would make it SLOWER than the unsafe read it replaces: the whole reason the
# unsafe read wins today is that the wrong answer is the cheap one, so the right
# answer must not cost a fetch. Every git call carries `--no-optional-locks`
# (git-LEVEL, before the subcommand — the `status --no-optional-locks` spelling
# is an `error: unknown option` that a `2>/dev/null` renders as a clean result)
# and `-c core.quotePath=false`.
#
# ── EXIT CODES (executed mode) ──────────────────────────────────────────────
#
#   exists <ref> <path>              0 FOUND (a blob) | 1 MISSING_AT_REF | 2 UNKNOWN
#   cat    <ref> <path>              0 FOUND, content on stdout | 1 MISSING_AT_REF | 2 UNKNOWN
#   grep   <ref> <pathspec> <pat>    0 match | 1 no match, pathspec VERIFIED
#                                    non-empty | 2 UNKNOWN | 3 PATHSPEC_EMPTY
#   sha    <ref>                     0 resolved, sha on stdout | 2 UNKNOWN
#
# 2 is UNKNOWN throughout, matching scripts/reach-grep.sh's rule ("Either exits
# 2, never 0"). 3 is never folded into 1: collapsing MISSING_AT_REF or
# PATHSPEC_EMPTY into "no match" is the entire defect this file exists to stop.

# ── git scope isolation (check #30) ─────────────────────────────────────────
#
# An inherited GIT_DIR skips repository discovery, so `git -C <root>` never
# applies to it and every read below would answer about ANOTHER repository —
# well-formed, confident, and wrong. The strip also clears GIT_LITERAL_PATHSPECS
# / GIT_GLOB_PATHSPECS / GIT_NOGLOB_PATHSPECS / GIT_ICASE_PATHSPECS, which is
# why the `grep` verb can trust an explicit `:(glob)` prefix to mean what it
# says and the other verbs cannot have a path turned into a glob underneath
# them. Stripped once, here, before any git runs.
#
# Absence alone is NOT danger: with no scope-redirecting variable set there is
# nothing to strip. So FATAL only when the lib is unusable AND such a variable
# is actually present — the same shape lib/tree-identity.sh uses.
_pin_gitscope_lib="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/git-scope.sh"
if [ -r "$_pin_gitscope_lib" ]; then
  # shellcheck source=git-scope.sh
  . "$_pin_gitscope_lib"
fi
if declare -F git_scope_strip >/dev/null 2>&1; then
  if ! git_scope_strip; then
    echo "pinned-read: FATAL - git_scope_strip could not clear a scope-redirecting git variable (named above), so every read below would describe ANOTHER repository. Refusing." >&2
    return 2 2>/dev/null || exit 2
  fi
else
  if [ -n "${GIT_DIR+s}${GIT_WORK_TREE+s}${GIT_COMMON_DIR+s}" ]; then
    echo "pinned-read: FATAL - lib/git-scope.sh is not usable ($_pin_gitscope_lib) AND this process carries a scope-redirecting git variable (GIT_DIR / GIT_WORK_TREE / GIT_COMMON_DIR), so every read below would describe ANOTHER repository. Refusing." >&2
    return 2 2>/dev/null || exit 2
  fi
  echo "pinned-read: WARNING - lib/git-scope.sh is not usable ($_pin_gitscope_lib); continuing because this process carries no GIT_DIR / GIT_WORK_TREE / GIT_COMMON_DIR, so there is nothing to strip. A scope-redirecting variable inherited LATER would NOT be caught." >&2
fi
unset _pin_gitscope_lib

# ── The sentinels ───────────────────────────────────────────────────────────
#
# One spelling, used by every field that can fail to resolve. Deliberately the
# plain token rather than a per-run nonce: a nonce would make two measurements
# of the same unresolvable read compare DIFFERENT, which is a different wrong
# answer (a false alarm rather than a false all-clear, but still an assertion
# about something nobody read). The fix for the collision is in the CONSUMER —
# see the ⚠️ on the grammar above.
PIN_UNKNOWN="UNKNOWN"

PIN_STATE_FOUND="FOUND"
PIN_STATE_MISSING="MISSING_AT_REF"
PIN_STATE_PATHSPEC_EMPTY="PATHSPEC_EMPTY"
PIN_STATE_UNKNOWN="UNKNOWN"

# Exit codes, named so a caller reads intent rather than an integer.
PIN_RC_FOUND=0
PIN_RC_MISSING=1
PIN_RC_UNKNOWN=2
PIN_RC_PATHSPEC_EMPTY=3

# The digests of NOTHING. Exported so a consumer that inherited a hash from
# somewhere else can recognise the collision rather than rediscovering it.
PIN_EMPTY_BLOB="e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
PIN_EMPTY_SHA256="e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"

# ── Internals ───────────────────────────────────────────────────────────────

# _pin_git <root> <args...> — every git call in this file goes through here.
# `--no-optional-locks` is GIT-LEVEL and must precede the subcommand: the
# `status --no-optional-locks` spelling is `error: unknown option`, which a
# `2>/dev/null` renders as a clean empty result.
_pin_git() {
  local root="$1"; shift
  git --no-optional-locks -c core.quotePath=false -C "$root" "$@"
}

# _pin_now — the measurement timestamp, UTC, seconds.
_pin_now() { date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || printf '%s' "$PIN_UNKNOWN"; }

# _pin_encode <s> — percent-encode whitespace and `%` so a path is one token.
# Only three sequences are produced (%25 first, so encoding is injective), which
# keeps the decoder trivial and keeps ordinary paths byte-identical.
_pin_encode() {
  local s="${1-}"
  s="${s//%/%25}"
  s="${s// /%20}"
  s="${s//$'\t'/%09}"
  printf '%s' "$s"
}

# _pin_emit <ref> <sha> <path> <state> <type>
#   ONE pin: line, to STDERR, BEFORE any stdout — the reach-grep.sh discipline,
#   so a `| head` cannot hide it and a caller that reads only stdout still gets
#   the provenance on the channel it did not filter.
_pin_emit() {
  printf 'pin: ref=%s sha=%s path=%s state=%s type=%s measured=%s\n' \
    "$(_pin_encode "${1-}")" "${2-$PIN_UNKNOWN}" "$(_pin_encode "${3-}")" \
    "${4-$PIN_UNKNOWN}" "${5-$PIN_UNKNOWN}" "$(_pin_now)" >&2
}

# _pin_reject_relative <path> — `<rev>:./x` and `<rev>:../x` are git's
# CWD-RELATIVE spelling, so the same argument names different content depending
# on where the caller stood. An absolute path is not a repo path at all. Both
# are rejected as UNKNOWN rather than silently resolved, because "it resolved to
# something" is exactly the failure mode here.
_pin_reject_relative() {
  case "${1-}" in
    ./*|../*|/*|.|..) return 1 ;;
    "")               return 1 ;;
    *)                return 0 ;;
  esac
}

# ── Public API ──────────────────────────────────────────────────────────────

# pin_sha <root> <ref>
#   Resolve <ref> to a commit sha ONCE. Prints the sha on stdout; exits
#   PIN_RC_UNKNOWN and prints nothing when it does not resolve.
#
#   `^{commit}` is load-bearing: it rejects a tag object or a tree that would
#   otherwise resolve to something that is not a commit, and it makes "did not
#   resolve" a single well-defined branch.
pin_sha() {
  local root="${1-}" ref="${2-}" sha
  sha="$(_pin_git "$root" rev-parse --verify -q "${ref}^{commit}" 2>/dev/null)" || sha=""
  if [ -z "$sha" ]; then
    return "$PIN_RC_UNKNOWN"
  fi
  printf '%s\n' "$sha"
  return 0
}

# pin_exists <root> <ref> <path>
#   0 FOUND (the path is a BLOB at that ref) | 1 MISSING_AT_REF | 2 UNKNOWN.
#
#   The oracle is `git cat-file -t <sha>:<path>` == `blob`, NOT `cat-file -e`:
#   measured, `-e` returns 0 for a DIRECTORY, which is the false-positive path
#   this whole file is about. A directory is reported MISSING_AT_REF with
#   `type=tree`, so the caller can tell "not there" from "there, but not a file".
pin_exists() {
  local root="${1-}" ref="${2-}" path="${3-}" sha typ
  if ! _pin_reject_relative "$path"; then
    _pin_emit "$ref" "$PIN_UNKNOWN" "$path" "$PIN_STATE_UNKNOWN" "$PIN_UNKNOWN"
    echo "pinned-read: refusing '$path' -- a path that is empty, absolute, or CWD-relative ('./', '../') does not name repo content at a ref; git would resolve it against the current directory instead" >&2
    return "$PIN_RC_UNKNOWN"
  fi
  sha="$(pin_sha "$root" "$ref")" || {
    _pin_emit "$ref" "$PIN_UNKNOWN" "$path" "$PIN_STATE_UNKNOWN" "$PIN_UNKNOWN"
    echo "pinned-read: could not resolve ref '$ref' in '$root' -- UNKNOWN, not MISSING" >&2
    return "$PIN_RC_UNKNOWN"
  }
  _pin_exists_at_sha "$root" "$ref" "$sha" "$path"
}

# _pin_exists_at_sha <root> <ref-as-given> <sha> <path>
#   The half of `pin_exists` that runs AFTER the ref is resolved, so a caller
#   holding a sha never resolves twice.
#
#   ⚠️ This split is not tidiness. `pin_cat` used to call `pin_exists` and then
#   `pin_sha` again, which is TWO `rev-parse` calls on the same ref -- and a
#   branch that moved between them would have the existence check and the read
#   land on DIFFERENT TREES. That is precisely the hole "resolve the ref exactly
#   once" exists to close, reopened inside the helper that closes it.
_pin_exists_at_sha() {
  local root="${1-}" ref="${2-}" sha="${3-}" path="${4-}" typ
  typ="$(_pin_git "$root" cat-file -t "${sha}:${path}" 2>/dev/null)" || typ=""
  case "$typ" in
    blob)
      _pin_emit "$ref" "$sha" "$path" "$PIN_STATE_FOUND" "blob"
      return "$PIN_RC_FOUND" ;;
    "")
      _pin_emit "$ref" "$sha" "$path" "$PIN_STATE_MISSING" "none"
      return "$PIN_RC_MISSING" ;;
    *)
      # A tree (or, in principle, a commit for a submodule): present at the ref,
      # but not readable as a file. Reported MISSING with the real type rather
      # than FOUND, because every caller of this helper is about to READ it.
      _pin_emit "$ref" "$sha" "$path" "$PIN_STATE_MISSING" "$typ"
      return "$PIN_RC_MISSING" ;;
  esac
}

# pin_cat <root> <ref> <path>
#   0 FOUND, content on stdout | 1 MISSING_AT_REF | 2 UNKNOWN.
#
#   Existence is established FIRST, and content reaches stdout ONLY in state 0.
#   That is the property that makes this safe to pipe: a caller that ignores the
#   exit code and pipes into `grep -c`, `sha256sum` or `hash-object` gets an
#   empty stream in every non-FOUND state, which is what it would have got from
#   the unsafe idiom — but it also gets a `pin:` line on stderr saying so, and a
#   non-zero exit if it ever looks.
pin_cat() {
  local root="${1-}" ref="${2-}" path="${3-}" sha rc
  if ! _pin_reject_relative "$path"; then
    _pin_emit "$ref" "$PIN_UNKNOWN" "$path" "$PIN_STATE_UNKNOWN" "$PIN_UNKNOWN"
    echo "pinned-read: refusing '$path' -- a path that is empty, absolute, or CWD-relative ('./', '../') does not name repo content at a ref; git would resolve it against the current directory instead" >&2
    return "$PIN_RC_UNKNOWN"
  fi
  # ONE resolution, reused by both the existence check and the read below --
  # see the warning on _pin_exists_at_sha.
  sha="$(pin_sha "$root" "$ref")" || {
    _pin_emit "$ref" "$PIN_UNKNOWN" "$path" "$PIN_STATE_UNKNOWN" "$PIN_UNKNOWN"
    echo "pinned-read: could not resolve ref '$ref' in '$root' -- UNKNOWN, not MISSING" >&2
    return "$PIN_RC_UNKNOWN"
  }
  _pin_exists_at_sha "$root" "$ref" "$sha" "$path"; rc=$?
  [ "$rc" -eq "$PIN_RC_FOUND" ] || return "$rc"
  # `cat-file blob` and not `show`: `show` applies a pager, textconv and diff
  # config, and would print a tree listing for a directory. This prints bytes.
  _pin_git "$root" cat-file blob "${sha}:${path}" 2>/dev/null || return "$PIN_RC_UNKNOWN"
  return "$PIN_RC_FOUND"
}

# pin_grep <root> <ref> <pathspec> <pattern> [extra grep args...]
#   0 match | 1 no match (pathspec VERIFIED non-empty) | 2 UNKNOWN
#   | 3 PATHSPEC_EMPTY.
#
#   The pathspec is probed FIRST with `git grep -l '' <sha> -- <pathspec>`,
#   which lists every file the pathspec selects AT THE REF. That probe is
#   provenance-pure — verified equal to `git ls-tree -r --name-only <sha>` with
#   no pathspec — and unlike `ls-files --with-tree` it cannot leak the index,
#   and unlike `ls-tree` it accepts `:(glob)` magic.
#
#   Zero selected files is state 3 and NEVER "no match". That is the direct
#   amendment to the `git grep <ref> -- <pathspec>` prescription: on its own,
#   git grep answers 1 with zero stderr for a typo'd pathspec and for a genuine
#   absence alike.
pin_grep() {
  local root="${1-}" ref="${2-}" pathspec="${3-}" pattern="${4-}"
  shift 4 2>/dev/null || { echo "pinned-read: grep needs <ref> <pathspec> <pattern>" >&2; return "$PIN_RC_UNKNOWN"; }
  local sha n rc
  sha="$(pin_sha "$root" "$ref")" || {
    _pin_emit "$ref" "$PIN_UNKNOWN" "$pathspec" "$PIN_STATE_UNKNOWN" "$PIN_UNKNOWN"
    echo "pinned-read: could not resolve ref '$ref' in '$root' -- UNKNOWN, not MISSING" >&2
    return "$PIN_RC_UNKNOWN"
  }
  # The pathspec probe. A `git grep -l ''` that FAILS to run (rc > 1) is
  # UNKNOWN, not an empty pathspec: an empty enumeration and a broken one must
  # not render the same [policy: silent-empty-is-unknown].
  local probe probe_rc=0
  probe="$(_pin_git "$root" grep -l '' "$sha" -- "$pathspec" 2>/dev/null)" || probe_rc=$?
  if [ "$probe_rc" -gt 1 ]; then
    _pin_emit "$ref" "$sha" "$pathspec" "$PIN_STATE_UNKNOWN" "$PIN_UNKNOWN"
    echo "pinned-read: pathspec probe failed (rc=$probe_rc) for '$pathspec' at $sha -- UNKNOWN, not empty" >&2
    return "$PIN_RC_UNKNOWN"
  fi
  n=$(printf '%s' "$probe" | grep -c . 2>/dev/null) || n=0
  if [ "$n" -eq 0 ]; then
    _pin_emit "$ref" "$sha" "$pathspec" "$PIN_STATE_PATHSPEC_EMPTY" "none"
    echo "pinned-read: pathspec '$pathspec' selects NO files at $sha -- this is PATHSPEC_EMPTY (3), not 'no match' (1)" >&2
    return "$PIN_RC_PATHSPEC_EMPTY"
  fi
  _pin_emit "$ref" "$sha" "$pathspec" "$PIN_STATE_FOUND" "blob"
  rc=0
  _pin_git "$root" grep "$@" -e "$pattern" "$sha" -- "$pathspec" || rc=$?
  # git grep's own 0/1; anything past 1 means the search did not complete.
  [ "$rc" -gt 1 ] && rc="$PIN_RC_UNKNOWN"
  return "$rc"
}

# ── Executed mode ───────────────────────────────────────────────────────────

_pin_usage() {
  cat <<'USAGE'
pinned-read.sh — read a ref, and never confuse "absent from the ref" with "no match".

  pinned-read.sh [--root <dir>] exists <ref> <path>
      0 FOUND (a blob at that ref) | 1 MISSING_AT_REF | 2 UNKNOWN

  pinned-read.sh [--root <dir>] cat <ref> <path>
      Content on stdout, ONLY in state 0.
      0 FOUND | 1 MISSING_AT_REF | 2 UNKNOWN

  pinned-read.sh [--root <dir>] grep <ref> <pathspec> <pattern> [git-grep args...]
      0 match | 1 no match (pathspec verified non-empty)
      | 2 UNKNOWN | 3 PATHSPEC_EMPTY

  pinned-read.sh [--root <dir>] sha <ref>
      The resolved commit sha on stdout, for pinning several reads to one tree.
      0 resolved | 2 UNKNOWN

  --root <dir>   the checkout to read through (default: the current directory)
  -h, --help     this text

Every verb prints ONE `pin:` line to stderr, before any stdout, naming the ref
AS GIVEN and the sha it was RESOLVED TO — so "names a ref" and "was read through
that ref" cannot diverge.

Exit 2 is UNKNOWN, never 0: "I could not resolve the ref" and "the file is not
there" are different answers, and 3 is never folded into 1.

  # instead of:  git show origin/main:path/to/f.md | grep -c PATTERN
  bash scripts/lib/pinned-read.sh cat origin/main path/to/f.md | grep -c PATTERN
  # ... and check the exit code, which now survives the pipe on stderr.
USAGE
}

_pin_main() {
  local root=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --root)    [ $# -ge 2 ] || { echo "pinned-read: --root needs a value" >&2; return 2; }; root="$2"; shift 2 ;;
      -h|--help) _pin_usage; return 0 ;;
      --)        shift; break ;;
      -*)        echo "pinned-read: unknown option '$1'" >&2; _pin_usage >&2; return 2 ;;
      *)         break ;;
    esac
  done
  [ -n "$root" ] || root="$PWD"
  [ $# -ge 1 ] || { echo "pinned-read: no verb given" >&2; _pin_usage >&2; return 2; }

  local verb="$1"; shift
  case "$verb" in
    exists) [ $# -eq 2 ] || { echo "pinned-read: exists needs <ref> <path>" >&2; return 2; }
            pin_exists "$root" "$1" "$2" ;;
    cat)    [ $# -eq 2 ] || { echo "pinned-read: cat needs <ref> <path>" >&2; return 2; }
            pin_cat "$root" "$1" "$2" ;;
    grep)   [ $# -ge 3 ] || { echo "pinned-read: grep needs <ref> <pathspec> <pattern>" >&2; return 2; }
            pin_grep "$root" "$@" ;;
    sha)    [ $# -eq 1 ] || { echo "pinned-read: sha needs <ref>" >&2; return 2; }
            pin_sha "$root" "$1" ;;
    *)      echo "pinned-read: unknown verb '$verb'" >&2; _pin_usage >&2; return 2 ;;
  esac
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  set -uo pipefail
  _pin_main "$@"
  exit $?
fi
