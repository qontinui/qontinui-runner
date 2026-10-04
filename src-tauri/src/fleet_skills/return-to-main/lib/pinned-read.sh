#!/usr/bin/env bash
# pinned-read.sh — the fleet's ONE pinned read: FOUND / MISSING_AT_REF / UNKNOWN.
#
# SOURCE this for the functions, or EXECUTE it for the five verbs. Both modes
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
# mutate the tree it is measuring. The ONE write anywhere in this file is
# `cat ... --save <file>`, and it writes exactly two files, both named by the
# CALLER: `<file>` and `<file>.pin`. It never writes an object, a ref, the
# index or the working tree of the repository it reads -- and it never writes
# at all outside state FOUND, and must never make the network call that
# would make it SLOWER than the unsafe read it replaces: the whole reason the
# unsafe read wins today is that the wrong answer is the cheap one, so the right
# answer must not cost a fetch. Every git call carries `--no-optional-locks`
# (git-LEVEL, before the subcommand — the `status --no-optional-locks` spelling
# is an `error: unknown option` that a `2>/dev/null` renders as a clean result)
# and `-c core.quotePath=false`.
#
# ── A CACHED BODY CARRIES ITS SHA: `--save` AND `recheck` ───────────────────
#
# Plan 2026-09-20-a-probe-that-could-not-answer-is-published-as-a-measurement,
# Phase 4 (sub-shape C). Finding b3d269a4: the freshness check was a diff of a
# NEW worktree against `origin/main` -- new-vs-new, so it could not fail -- and
# the phase detail was then read from a scratchpad copy saved 16 h earlier. The
# check and the consumer read DIFFERENT sources. No re-diff closes that: the
# copy is not in any tree a diff looks at. What closes it is the copy carrying
# the sha it was read through, and a re-read that asks the REF again.
#
#   cat <ref> <path> --save <file>
#     Resolves once, exactly as `cat`, and ONLY in state FOUND writes the blob's
#     bytes to <file> and a sidecar <file>.pin:
#
#       pin: ref=... sha=<sha> path=... state=FOUND type=blob measured=...
#       blob=<the blob id those bytes ARE>
#       root=<the checkout, absolute>
#       ref=<the ref as given, raw>
#       path=<the path, raw>
#
#     Line 1 is the ordinary `pin:` line; the four `key=value` lines are what
#     `recheck` needs raw (unencoded), one per line. Both files are written to a
#     temporary name beside the target and renamed, content FIRST: a crash
#     between the two renames leaves new bytes under the old sidecar, which
#     `recheck` reports EDITED -- detected, never a silent CURRENT. A path, ref
#     or file name containing a newline is refused (UNKNOWN), because it would
#     split a sidecar line. Stdout is empty: the content went to the file.
#     The `pin:` line on STDERR agrees with the exit code: `state=FOUND` is
#     emitted only AFTER both renames succeeded, and a FOUND path whose blob
#     id, checkout path or write then failed emits `state=UNKNOWN` instead --
#     never a FOUND line followed by exit 2.
#
#   recheck <file>
#     Reads <file>.pin, resolves `ref` AGAIN in `root` -- never reuses the saved
#     sha, which would make the recheck as unable to fail as the diff it
#     replaces -- and compares three blob ids: the saved one, the one at
#     `<new sha>:<path>`, and `git hash-object --no-filters <file>` (raw bytes;
#     without `--no-filters` a CRLF or clean-filter config would hash the bytes
#     git WOULD store, not the bytes on disk). Local != saved is EDITED, whatever
#     the ref says. Otherwise saved == current is CURRENT, and saved != current
#     is STALE -- including a path now absent from the ref (`missing_at_ref`)
#     or a tree there (`not_a_blob_at_ref`): the ref resolved and the path was
#     MEASURED, so that is a stale copy, not "could not tell"; folding it into
#     UNKNOWN would collapse a measured absence into an unmeasured one.
#
#     NO FETCH. `as_of=<sha>` on its line is what the ref resolved to in that
#     checkout NOW, so CURRENT claims "current as of this checkout's last fetch"
#     and says which sha that was. A ref given as a raw sha re-resolves to
#     itself, so such a copy can only ever be CURRENT or EDITED -- which is the
#     truth about a copy pinned to an immutable commit.
#
#     It prints ONE `pin-recheck:` line to stderr (registered beside the `pin:`
#     grammar as `PIN_RECHECK_LINE_RE`; see the comment there for why it is a
#     second line rather than more `pin:` states) and nothing on stdout.
#
# ── EXIT CODES (executed mode) ──────────────────────────────────────────────
#
#   exists <ref> <path>              0 FOUND (a blob) | 1 MISSING_AT_REF | 2 UNKNOWN
#   cat    <ref> <path>              0 FOUND, content on stdout | 1 MISSING_AT_REF | 2 UNKNOWN
#   grep   <ref> <pathspec> <pat>    0 match | 1 no match, pathspec VERIFIED
#                                    non-empty | 2 UNKNOWN | 3 PATHSPEC_EMPTY
#   sha    <ref>                     0 resolved, sha on stdout | 2 UNKNOWN
#   cat    <ref> <path> --save <file>
#                                    0 FOUND, content written to <file> and its
#                                    provenance to <file>.pin; NOTHING on stdout
#                                    | 1 MISSING_AT_REF | 2 UNKNOWN -- and in
#                                    every non-zero state NEITHER file is written
#   recheck <file>                   0 CURRENT | 1 STALE (the ref moved the blob,
#                                    or the path is gone from it) | 1 EDITED (the
#                                    local bytes are not the saved blob)
#                                    | 2 UNKNOWN (no sidecar, the checkout gone,
#                                    the ref unresolvable, the copy missing)
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

# _pin_line <ref> <sha> <path> <state> <type>
#   ONE pin: line, on stdout -- the text only. `_pin_emit` sends it to stderr;
#   `--save` also writes it as the sidecar's first line.
_pin_line() {
  printf 'pin: ref=%s sha=%s path=%s state=%s type=%s measured=%s\n' \
    "$(_pin_encode "${1-}")" "${2-$PIN_UNKNOWN}" "$(_pin_encode "${3-}")" \
    "${4-$PIN_UNKNOWN}" "${5-$PIN_UNKNOWN}" "$(_pin_now)"
}

# _pin_emit <ref> <sha> <path> <state> <type>
#   ONE pin: line, to STDERR, BEFORE any stdout — the reach-grep.sh discipline,
#   so a `| head` cannot hide it and a caller that reads only stdout still gets
#   the provenance on the channel it did not filter.
_pin_emit() {
  _pin_line "$@" >&2
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

# _pin_abs <path> — the path made absolute against $PWD, lexically (no
# symlink resolution, no spawn). A relative sidecar `root=` or a relative copy
# name would mean something different from the next caller's directory.
_pin_abs() {
  case "${1-}" in
    /*|[A-Za-z]:/*) printf '%s' "$1" ;;
    *)              printf '%s/%s' "$PWD" "$1" ;;
  esac
}

# pin_cat_save <root> <ref> <path> <file>
#   0 FOUND, bytes written to <file> and provenance to <file>.pin (nothing on
#   stdout) | 1 MISSING_AT_REF | 2 UNKNOWN. In EVERY non-zero state neither
#   file is written -- the same "content only in state 0" rule as `pin_cat`,
#   applied to a file instead of a stream. See "A CACHED BODY CARRIES ITS SHA".
pin_cat_save() {
  local root="${1-}" ref="${2-}" path="${3-}" file="${4-}" sha rc oid abs_root line tmpc tmps typ
  if ! _pin_reject_relative "$path"; then
    _pin_emit "$ref" "$PIN_UNKNOWN" "$path" "$PIN_STATE_UNKNOWN" "$PIN_UNKNOWN"
    echo "pinned-read: refusing '$path' -- a path that is empty, absolute, or CWD-relative ('./', '../') does not name repo content at a ref; git would resolve it against the current directory instead" >&2
    return "$PIN_RC_UNKNOWN"
  fi
  case "$ref$path$file" in
    *$'\n'*)
      _pin_emit "$ref" "$PIN_UNKNOWN" "$path" "$PIN_STATE_UNKNOWN" "$PIN_UNKNOWN"
      echo "pinned-read: refusing --save -- a newline in the ref, path or file name would split a line of the <file>.pin sidecar" >&2
      return "$PIN_RC_UNKNOWN" ;;
  esac
  if [ -z "$file" ]; then
    _pin_emit "$ref" "$PIN_UNKNOWN" "$path" "$PIN_STATE_UNKNOWN" "$PIN_UNKNOWN"
    echo "pinned-read: --save needs a file name" >&2
    return "$PIN_RC_UNKNOWN"
  fi
  file="$(_pin_abs "$file")"
  if [ ! -d "${file%/*}" ] || [ -d "$file" ]; then
    _pin_emit "$ref" "$PIN_UNKNOWN" "$path" "$PIN_STATE_UNKNOWN" "$PIN_UNKNOWN"
    echo "pinned-read: --save target '$file' is a directory or sits in a directory that does not exist -- nothing written" >&2
    return "$PIN_RC_UNKNOWN"
  fi
  sha="$(pin_sha "$root" "$ref")" || {
    _pin_emit "$ref" "$PIN_UNKNOWN" "$path" "$PIN_STATE_UNKNOWN" "$PIN_UNKNOWN"
    echo "pinned-read: could not resolve ref '$ref' in '$root' -- UNKNOWN, not MISSING; nothing written" >&2
    return "$PIN_RC_UNKNOWN"
  }
  # NOT `_pin_exists_at_sha` on the FOUND path: it would emit `state=FOUND`
  # before the save below could still fail, and the caller would read a FOUND
  # line and an exit 2 for one call. The line is emitted once the outcome is
  # known. A non-blob delegates to it, for the MISSING line it already owns.
  typ="$(_pin_git "$root" cat-file -t "${sha}:${path}" 2>/dev/null)" || typ=""
  if [ "$typ" != blob ]; then
    _pin_exists_at_sha "$root" "$ref" "$sha" "$path"; rc=$?
    # The object store is immutable per sha, so a second look answering FOUND
    # is a flaking read, not a blob: UNKNOWN, with its own line.
    if [ "$rc" -eq "$PIN_RC_FOUND" ]; then
      _pin_emit "$ref" "$sha" "$path" "$PIN_STATE_UNKNOWN" "$PIN_UNKNOWN"
      echo "pinned-read: '${sha}:${path}' answered two different object types -- UNKNOWN; nothing written" >&2
      return "$PIN_RC_UNKNOWN"
    fi
    return "$rc"
  fi
  # The blob id, from the SAME resolved sha. The bytes are then read BY that id,
  # so what lands in <file> is by construction the blob the sidecar names.
  oid="$(_pin_git "$root" rev-parse --verify -q "${sha}:${path}" 2>/dev/null)" || oid=""
  abs_root="$(cd "$root" 2>/dev/null && pwd)" || abs_root=""
  if ! _pin_is_oid "$oid" || [ -z "$abs_root" ]; then
    _pin_emit "$ref" "$sha" "$path" "$PIN_STATE_UNKNOWN" "blob"
    echo "pinned-read: FOUND at $sha but the blob id or the checkout's absolute path did not resolve -- UNKNOWN; nothing written" >&2
    return "$PIN_RC_UNKNOWN"
  fi
  tmpc="$file.pin-save.$$"
  tmps="$file.pin.pin-save.$$"
  line="$(_pin_line "$ref" "$sha" "$path" "$PIN_STATE_FOUND" "blob")"
  if _pin_git "$root" cat-file blob "$oid" >"$tmpc" 2>/dev/null \
     && printf '%s\nblob=%s\nroot=%s\nref=%s\npath=%s\n' "$line" "$oid" "$abs_root" "$ref" "$path" >"$tmps" 2>/dev/null \
     && mv -f "$tmpc" "$file" 2>/dev/null \
     && mv -f "$tmps" "$file.pin" 2>/dev/null; then
    printf '%s\n' "$line" >&2
    return "$PIN_RC_FOUND"
  fi
  rm -f "$tmpc" "$tmps" 2>/dev/null
  _pin_emit "$ref" "$sha" "$path" "$PIN_STATE_UNKNOWN" "blob"
  echo "pinned-read: writing '$file' or its .pin sidecar failed -- UNKNOWN" >&2
  return "$PIN_RC_UNKNOWN"
}

# _pin_recheck_emit <file> <state> <ref> <path> <root> <saved_sha> <as_of>
#                   <saved_blob> <current_blob> <local_blob> <reason>
#   ONE pin-recheck: line to stderr. Empty fields render as the sentinel.
_pin_recheck_emit() {
  local u="$PIN_UNKNOWN"
  printf 'pin-recheck: file=%s state=%s ref=%s path=%s root=%s saved_sha=%s as_of=%s saved_blob=%s current_blob=%s local_blob=%s reason=%s measured=%s\n' \
    "$(_pin_encode "${1:-$u}")" "${2:-$u}" "$(_pin_encode "${3:-$u}")" \
    "$(_pin_encode "${4:-$u}")" "$(_pin_encode "${5:-$u}")" "${6:-$u}" \
    "${7:-$u}" "${8:-$u}" "${9:-$u}" "${10:-$u}" "${11:-$u}" "$(_pin_now)" >&2
}

# _pin_is_oid <s> — a 40- or 64-hex object id (SHA-1 or SHA-256 repository).
_pin_is_oid() {
  [[ "${1-}" =~ ^[0-9a-f]{40}([0-9a-f]{24})?$ ]]
}

# pin_recheck <file>
#   0 CURRENT | 1 STALE | 1 EDITED | 2 UNKNOWN. See "A CACHED BODY CARRIES ITS
#   SHA" for what each state compares, and why the ref is resolved AGAIN.
pin_recheck() {
  local file="${1-}" side l pinline="" s_blob="" s_root="" s_ref="" s_path="" saved_sha=""
  local new_sha local_blob typ cur reason
  if [ -z "$file" ]; then
    _pin_recheck_emit "" "$PIN_STATE_UNKNOWN" "" "" "" "" "" "" "" "" no_sidecar
    echo "pinned-read: recheck needs <file>" >&2
    return "$PIN_RC_UNKNOWN"
  fi
  file="$(_pin_abs "$file")"
  side="$file.pin"
  if [ ! -r "$side" ]; then
    _pin_recheck_emit "$file" "$PIN_STATE_UNKNOWN" "" "" "" "" "" "" "" "" no_sidecar
    echo "pinned-read: '$side' is absent or unreadable -- this copy carries no provenance, so whether it is still the ref's is UNKNOWN" >&2
    return "$PIN_RC_UNKNOWN"
  fi
  # First occurrence of each key wins; the `pin:` line is line 1 by contract.
  while IFS= read -r l || [ -n "$l" ]; do
    case "$l" in
      "pin: "*) [ -n "$pinline" ] || pinline="$l" ;;
      blob=*)   [ -n "$s_blob" ]  || s_blob="${l#blob=}" ;;
      root=*)   [ -n "$s_root" ]  || s_root="${l#root=}" ;;
      ref=*)    [ -n "$s_ref" ]   || s_ref="${l#ref=}" ;;
      path=*)   [ -n "$s_path" ]  || s_path="${l#path=}" ;;
    esac
  done <"$side"
  # 40 OR 64 hex -- a SHA-256 repository's commit ids are 64, and `_pin_is_oid`
  # accepts both for the blob; a 40-only match here would call every such
  # sidecar malformed.
  [[ "$pinline" =~ \ sha=([0-9a-f]{40}([0-9a-f]{24})?)\  ]] && saved_sha="${BASH_REMATCH[1]}"
  if [ -z "$saved_sha" ] || ! _pin_is_oid "$s_blob" || [ -z "$s_root" ] \
     || [ -z "$s_ref" ] || ! _pin_reject_relative "$s_path"; then
    _pin_recheck_emit "$file" "$PIN_STATE_UNKNOWN" "$s_ref" "$s_path" "$s_root" "$saved_sha" "" "" "" "" sidecar_malformed
    echo "pinned-read: '$side' does not carry a pin: line with a resolved sha plus blob=/root=/ref=/path= -- UNKNOWN" >&2
    return "$PIN_RC_UNKNOWN"
  fi
  if [ ! -f "$file" ]; then
    _pin_recheck_emit "$file" "$PIN_STATE_UNKNOWN" "$s_ref" "$s_path" "$s_root" "$saved_sha" "" "$s_blob" "" "" copy_missing
    echo "pinned-read: the copy '$file' is gone but its sidecar remains -- UNKNOWN" >&2
    return "$PIN_RC_UNKNOWN"
  fi
  if [ ! -d "$s_root" ]; then
    _pin_recheck_emit "$file" "$PIN_STATE_UNKNOWN" "$s_ref" "$s_path" "$s_root" "$saved_sha" "" "$s_blob" "" "" root_gone
    echo "pinned-read: the checkout '$s_root' this copy was read through no longer exists -- UNKNOWN, not current and not stale" >&2
    return "$PIN_RC_UNKNOWN"
  fi
  # ⚠️ THE RE-RESOLUTION. `saved_sha` is what the ref WAS; comparing against it
  # would pass every copy forever -- the new-vs-new diff of b3d269a4 again.
  new_sha="$(pin_sha "$s_root" "$s_ref")" || new_sha=""
  if [ -z "$new_sha" ]; then
    _pin_recheck_emit "$file" "$PIN_STATE_UNKNOWN" "$s_ref" "$s_path" "$s_root" "$saved_sha" "" "$s_blob" "" "" ref_unresolvable
    echo "pinned-read: ref '$s_ref' no longer resolves in '$s_root' -- UNKNOWN" >&2
    return "$PIN_RC_UNKNOWN"
  fi
  local_blob="$(_pin_git "$s_root" hash-object --no-filters -- "$file" 2>/dev/null)" || local_blob=""
  if ! _pin_is_oid "$local_blob"; then
    _pin_recheck_emit "$file" "$PIN_STATE_UNKNOWN" "$s_ref" "$s_path" "$s_root" "$saved_sha" "$new_sha" "$s_blob" "" "" hash_failed
    echo "pinned-read: could not hash '$file' -- UNKNOWN" >&2
    return "$PIN_RC_UNKNOWN"
  fi
  typ="$(_pin_git "$s_root" cat-file -t "${new_sha}:${s_path}" 2>/dev/null)" || typ=""
  case "$typ" in
    blob) cur="$(_pin_git "$s_root" rev-parse --verify -q "${new_sha}:${s_path}" 2>/dev/null)" || cur=""
          _pin_is_oid "$cur" || cur="$PIN_UNKNOWN"
          reason=ref_moved_blob ;;
    "")   cur="none"; reason=missing_at_ref ;;
    *)    cur="none"; reason=not_a_blob_at_ref ;;
  esac
  if [ "$local_blob" != "$s_blob" ]; then
    _pin_recheck_emit "$file" EDITED "$s_ref" "$s_path" "$s_root" "$saved_sha" "$new_sha" "$s_blob" "$cur" "$local_blob" local_bytes_differ
    return 1
  fi
  if [ "$cur" = "$PIN_UNKNOWN" ]; then
    _pin_recheck_emit "$file" "$PIN_STATE_UNKNOWN" "$s_ref" "$s_path" "$s_root" "$saved_sha" "$new_sha" "$s_blob" "" "$local_blob" hash_failed
    echo "pinned-read: '${new_sha}:${s_path}' is a blob but its id did not resolve -- UNKNOWN" >&2
    return "$PIN_RC_UNKNOWN"
  fi
  if [ "$cur" = "$s_blob" ]; then
    _pin_recheck_emit "$file" CURRENT "$s_ref" "$s_path" "$s_root" "$saved_sha" "$new_sha" "$s_blob" "$cur" "$local_blob" blob_unchanged
    return 0
  fi
  _pin_recheck_emit "$file" STALE "$s_ref" "$s_path" "$s_root" "$saved_sha" "$new_sha" "$s_blob" "$cur" "$local_blob" "$reason"
  return 1
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

  pinned-read.sh [--root <dir>] cat <ref> <path> --save <file>
      Content to <file> and its provenance (the pin: line, blob=, root=, ref=,
      path=) to <file>.pin, ONLY in state 0; nothing on stdout, and neither
      file is written in any other state.
      0 FOUND | 1 MISSING_AT_REF | 2 UNKNOWN

  pinned-read.sh recheck <file>
      Is a --save'd copy still the ref's? Re-resolves the sidecar's ref in its
      checkout (no fetch; the line says as_of=<sha>) and compares blob ids.
      0 CURRENT | 1 STALE (the ref moved the blob) | 1 EDITED (the local bytes
      differ from the saved blob) | 2 UNKNOWN (no sidecar, checkout gone, ref
      unresolvable, copy missing)

  pinned-read.sh [--root <dir>] grep <ref> <pathspec> <pattern> [git-grep args...]
      0 match | 1 no match (pathspec verified non-empty)
      | 2 UNKNOWN | 3 PATHSPEC_EMPTY

  pinned-read.sh [--root <dir>] sha <ref>
      The resolved commit sha on stdout, for pinning several reads to one tree.
      0 resolved | 2 UNKNOWN

  --root <dir>   the checkout to read through (default: the current directory)
  -h, --help     this text

Every read verb prints ONE `pin:` line to stderr (`recheck` prints one
`pin-recheck:` line instead), before any stdout, naming the ref
AS GIVEN and the sha it was RESOLVED TO — so "names a ref" and "was read through
that ref" cannot diverge.

Exit 2 is UNKNOWN, never 0: "I could not resolve the ref" and "the file is not
there" are different answers, and 3 is never folded into 1.

  # instead of:  git show origin/main:path/to/f.md | grep -c PATTERN
  bash scripts/lib/pinned-read.sh cat origin/main path/to/f.md | grep -c PATTERN
  # ... and check the exit code, which now survives the pipe on stderr.

  # instead of:  git show origin/main:path/to/f.md > /tmp/f.md   (no sha kept)
  bash pinned-read.sh cat origin/main path/to/f.md --save /tmp/f.md
  bash pinned-read.sh recheck /tmp/f.md    # before trusting the copy later
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
    cat)    if [ $# -eq 2 ]; then
              pin_cat "$root" "$1" "$2"
            elif [ $# -eq 4 ] && [ "$3" = "--save" ]; then
              pin_cat_save "$root" "$1" "$2" "$4"
            else
              echo "pinned-read: cat needs <ref> <path> [--save <file>]" >&2; return 2
            fi ;;
    recheck) [ $# -eq 1 ] || { echo "pinned-read: recheck needs <file>" >&2; return 2; }
            pin_recheck "$1" ;;
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
