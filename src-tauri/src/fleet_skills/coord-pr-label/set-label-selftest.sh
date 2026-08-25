#!/usr/bin/env bash
# Self-test for set-label.sh's pre-send validation.
#
# Runs the REAL script (via `--dry-run`, which stops before gh and coord), so
# this exercises the shipped validator rather than a copy of it. No network and
# no real gh: a stub shadows it on PATH purely as a tripwire (see below), and
# `--dry-run` must never reach even that.
#
# Covers both directions, because a guard proven only against known-bad input
# is indistinguishable from a guard that rejects everything:
#   - known-BAD: over-ceiling labels are rejected AND the error names the
#     owner-dropped short form that would fit;
#   - known-GOOD: the short form, a full form that fits, flag labels and the
#     same-repo `#<n>` arm all still pass;
#   - the 50/51-character BOUNDARY is asserted by length, not by eyeball, so a
#     repo rename cannot silently slide the corpus off the edge it is testing;
#   - `--dry-run` sends NOTHING: a PATH-shadowed `gh` stub records any call, and
#     the run is asserted to have left no such record.
#
# The PR coordinates below are deliberately unresolvable (`--pr 0` on a repo
# that does not exist). If a future edit ever moves the dry-run short-circuit
# BELOW the `gh pr edit` call, this test fails closed instead of adding real
# labels to a live PR on a developer box with an authed gh. The owner is still
# `qontinui`, because the short-form suggestion is owner-scoped.
#
# Usage: bash set-label-selftest.sh    (exit 0 = all cases classified correctly)

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$HERE/set-label.sh"

REPO="qontinui/does-not-exist-selftest"
PRNUM="0"

FAILURES=0
CHECKS=0

fail() { echo "FAIL: $*" >&2; FAILURES=$((FAILURES + 1)); }
ok()   { CHECKS=$((CHECKS + 1)); }

# ----- gh tripwire ------------------------------------------------------------
# A stub `gh` earlier on PATH than the real one, recording every invocation.
# `--dry-run` must never reach it.
#
# QONTINUI_AGENT_ID is exported deliberately. Without it the agent-id check
# (set-label.sh, just below the dry-run exit) would halt a regressed script
# BEFORE gh -- so on CI, which has no such variable, the no-send property would
# be enforced by that check rather than by the dry-run exit this test claims to
# pin, and the tripwire would be green by construction through the very
# regression it names. Exporting a dummy makes the dry-run exit the only thing
# between the corpus and `gh pr edit`.
#
# The stub exits NON-zero so a regressed script stops at
# `error: gh pr edit failed` instead of continuing into the coord POST -- which,
# on a box with a live local coord, would fire one real request per corpus
# entry. COORD_URL is pinned at a dead port for the same reason.
STUBDIR="$(mktemp -d)" || { echo "FAIL: mktemp -d failed; refusing to run with an unshadowed PATH" >&2; exit 1; }
SENTINEL="$STUBDIR/gh-was-called"
{
  echo '#!/usr/bin/env bash'
  echo 'echo "gh $*" >> "$(dirname "$0")/gh-was-called"'
  echo 'exit 1'
} > "$STUBDIR/gh"
chmod +x "$STUBDIR/gh"
PATH="$STUBDIR:$PATH"
export PATH
export QONTINUI_AGENT_ID="selftest-dummy"
export COORD_URL="http://127.0.0.1:1"
cleanup() { rm -rf "$STUBDIR"; }
trap cleanup EXIT

# run <label> -> sets RC and OUT (stdout+stderr combined)
run() {
  OUT="$(bash "$SCRIPT" --repo "$REPO" --pr "$PRNUM" --label "$1" --dry-run 2>&1)"
  RC=$?
}

expect_accept() {
  local label="$1"
  run "$label"
  if [[ $RC -ne 0 ]]; then
    fail "expected accept, got rc=$RC for \"$label\" :: $OUT"
  else
    ok
  fi
}

# expect_reject <label> <substring the error must contain>
expect_reject() {
  local label="$1" needle="$2"
  run "$label"
  if [[ $RC -eq 0 ]]; then
    fail "expected reject, got rc=0 for \"$label\""
  elif [[ "$OUT" != *"$needle"* ]]; then
    fail "reject message for \"$label\" lacks \"$needle\" :: $OUT"
  else
    ok
  fi
}

# expect_absent <label> <substring the error must NOT contain>
expect_absent() {
  local label="$1" needle="$2"
  run "$label"
  if [[ $RC -eq 0 ]]; then
    fail "expected reject, got rc=0 for \"$label\""
  elif [[ "$OUT" == *"$needle"* ]]; then
    fail "reject message for \"$label\" should not contain \"$needle\" :: $OUT"
  else
    ok
  fi
}

# expect_len <label> <expected length> -- anchors the boundary corpus.
expect_len() {
  local label="$1" want="$2"
  if [[ ${#label} -ne $want ]]; then
    fail "corpus drift: \"$label\" is ${#label} chars, expected $want"
  else
    ok
  fi
}

# ----- boundary anchors -------------------------------------------------------
# Exactly at the ceiling (must pass) and exactly one over (must fail). If a repo
# is ever renamed these two assertions fail loudly rather than quietly testing
# some other length.
AT_CEILING="coord:stacked-on=qontinui/qontinui-supervisor#1234"
ONE_OVER="coord:upstream-of=qontinui/qontinui-supervisor#1234"
expect_len "$AT_CEILING" 50
expect_len "$ONE_OVER" 51

expect_accept "$AT_CEILING"
expect_reject "$ONE_OVER" "51 characters"
# ...and the suggestion must be the owner-dropped form of that same label.
expect_reject "$ONE_OVER" 'coord:upstream-of=qontinui-supervisor#1234'

# ----- known-BAD: over the ceiling --------------------------------------------
# The case that first hit, 2026-08-19 (claude-config#296 -> dev-notes#167).
expect_reject "coord:downstream-of=qontinui/qontinui-claude-config#296" \
  'coord:downstream-of=qontinui-claude-config#296'
expect_reject "coord:downstream-of=qontinui/qontinui-dev-notes#1234" \
  'coord:downstream-of=qontinui-dev-notes#1234'
# The mis-signpost warning is the point of the guard, so assert it is present.
expect_reject "coord:downstream-of=qontinui/qontinui-claude-config#296" \
  "not found"
# A non-dep key has no short form: report the overflow, suggest nothing bogus.
expect_reject "coord:requires-tag=ts-v0.0.0-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" \
  "shorten the value"

# ----- the suggestion must never be a label the script itself rejects ---------
# A FOREIGN owner is not ours to drop: coord canonicalizes a bare name to the
# TENANT's owner, so the "short form" would silently retarget the edge at a
# different repo. Suggest nothing.
expect_absent "coord:downstream-of=some-other-org/qontinui-dev-notes#1234" \
  "drop the owner"
expect_reject "coord:downstream-of=some-other-org/qontinui-dev-notes#1234" \
  "shorten the value"
# An owner with an EMPTY repo part would shorten to `coord:upstream-of=#<n>`,
# which validate_label rejects ("missing repo"). Suggest nothing.
#
# The PR number is sized so the FULL form is 51 (over the ceiling, so the guard
# runs) while the short form is 42 (under it). An earlier 32-digit version of
# this case was vacuous: the short form was 51 too, so the call site's own
# `${#SHORT} -le $GH_LABEL_MAX` rejected it and the assertion passed whether or
# not short_form's guards existed at all.
EMPTY_BARE="coord:upstream-of=qontinui/#12345678901234567890123"
expect_len "$EMPTY_BARE" 51
expect_absent "$EMPTY_BARE" "drop the owner"
# `stacked-on` is what makes the empty-`bare` guard load-bearing on its own:
# validate_label ACCEPTS `coord:stacked-on=#<n>` (that is the same-repo arm), so
# without the guard the suggestion would be well-formed and WRONG -- a
# cross-repo edge silently rewritten as a same-repo one.
EMPTY_BARE_STACKED="coord:stacked-on=qontinui/#123456789012345678901234"
expect_len "$EMPTY_BARE_STACKED" 51
expect_absent "$EMPTY_BARE_STACKED" "drop the owner"
# Likewise the not-a-path guard: `qontinui/a/qontinui-devtools#1234` would
# shorten to `a/qontinui-devtools#1234`, which validate_label accepts (the repo
# part is non-empty AND both `/`-segments are) and which names a different repo.
NESTED_PATH="coord:upstream-of=qontinui/a/qontinui-devtools#1234"
expect_len "$NESTED_PATH" 51
expect_absent "$NESTED_PATH" "drop the owner"

# ----- known-GOOD: must still pass --------------------------------------------
expect_accept "coord:downstream-of=qontinui-claude-config#296"   # the short form
expect_accept "coord:upstream-of=qontinui/qontinui-schemas#42"   # full form, fits
expect_accept "coord:stacked-on=#42"                             # same-repo arm
expect_accept "coord:stacked-on=qontinui-web#748"
expect_accept "coord:blocked"
expect_accept "coord:experimental"
expect_accept "coord:credibility-override"
expect_accept "coord:migrate-repair"
expect_accept "coord:merge-strategy=squash"
expect_accept "coord:requires-tag=ts-v*"

# ----- grammar rejections must be unchanged by the length guard ---------------
# Needles are exact: a loose "missing" would also match the length guard's own
# "missing-label problem" line.
expect_reject "coord:stacked-on=nohash"       'stacked-on: missing "#<pr_number>"'
expect_reject "coord:state=open"              "coord-set label"
expect_reject "coord:blocked-by=x"            "coord-set label"
expect_reject "coord:operator-review"         "retired label"
expect_reject "coord:version-bump=1"          "retired label"
expect_reject "coord:bogus=1"                 "unknown coord:* label key"
expect_reject "not-a-coord-label"             "must start with"
expect_reject "coord:downstream-of=repo#abc"  "must be int"

# pr_number is `parse::<i32>()` in coord, NOT `^[0-9]+$`. `abc` is the one
# input where those two agree, so it was the whole integer corpus and pinned
# nothing at the boundary. These cases pin the DOMAIN in both directions --
# overflow is rejected (the green-light-then-server-refuses class) and the
# signed forms coord accepts are not "improved on" here.
expect_accept "coord:upstream-of=repo#2147483647"      # i32::MAX
expect_reject "coord:upstream-of=repo#2147483648"      "must be int"
expect_reject "coord:upstream-of=repo#99999999999999999999" "must be int"
expect_accept "coord:upstream-of=repo#-2147483648"     # i32::MIN
expect_reject "coord:upstream-of=repo#-2147483649"     "must be int"
expect_accept "coord:upstream-of=repo#-1"              # coord accepts; mirror must too
expect_accept "coord:upstream-of=repo#+1"
expect_accept "coord:upstream-of=repo#007"             # leading zeros: 10# not octal
# `007` alone does NOT pin the `10#`: without it bash reads octal 7, which is
# still <= i32::MAX, so the verdict is unchanged. `008`/`09` are the
# discriminating cases -- an invalid octal digit makes bare `(( ))` a hard
# error, so dropping `10#` flips these to reject. Rust parses all of them.
expect_accept "coord:upstream-of=repo#008"
expect_accept "coord:upstream-of=repo#09"
expect_accept "coord:upstream-of=repo#0000000008"      # exactly 10 chars
# 11 zero-padded chars: pins that the strip loop runs BEFORE the `<= 10`
# length guard. `#0000000008` above is exactly 10 and squeaks under the
# guard even with the strip removed, so it does NOT pin the ordering.
expect_accept "coord:upstream-of=repo#00000000008"
# 2^64+5 wraps to 5 in bash 64-bit arithmetic, so without the length guard
# this would be ACCEPTED while coord rejects it. Pins the guard as covered
# behaviour rather than an unfalsifiable backstop.
expect_reject "coord:upstream-of=repo#18446744073709551621" "must be int"
expect_reject "coord:stacked-on=#2147483648"           "must be int"
expect_accept "coord:stacked-on=#2147483647"
expect_reject "coord:merge-strategy=bogus"    "must be one of squash|rebase|merge"
expect_reject "coord:requires-tag="           'value after "=" cannot be empty'
# `coord:priority` USED to fall through to the generic parameterised-label arm.
# coord has since grown a bespoke `PRIORITY_LABEL_ERR` that names the working
# alternative, and catches the parameterised form too -- so both spellings must
# now produce the bespoke message, not the generic one. The needle is the part
# that carries the fix, so a reworded preamble does not silently un-pin it.
expect_reject "coord:priority"                "must be set on the PR itself"
expect_reject "coord:priority=1"              "must be set on the PR itself"

# `coord:red-main-fix` is rejected by coord with the GENERIC message -- coord
# has no bespoke arm for it, so this mirror must not invent one. This case pins
# the ABSENCE of a bespoke arm: if someone adds one here without adding it to
# `labels_routes.rs` first, the mirror has drifted and this fails. The doctrine
# (why the label buys nothing, and to set it with `gh pr edit`) lives in
# SKILL.md, not in the validator.
expect_reject "coord:red-main-fix"            'parameterised labels need "=value"'

# `repo_segments_well_formed` -- an owner/repo value needs BOTH segments. These
# three were ACCEPTED by the mirror while coord refused them at the write
# surface: a pre-flight that green-lights a label the server rejects is worse
# than none. Covers both dep-label arms and stacked-on's non-empty repo form.
expect_reject "coord:upstream-of=/repo#1"     'empty owner or repo segment'
expect_reject "coord:downstream-of=qontinui/#1" 'empty owner or repo segment'
expect_reject "coord:stacked-on=/repo#1"      'empty owner or repo segment'

# ----- a bare flag must say which flag, not exit 1 silently -------------------
OUT="$(bash "$SCRIPT" --repo "$REPO" --pr "$PRNUM" --label 2>&1)"; RC=$?
if [[ $RC -eq 0 || "$OUT" != *"--label needs a value"* ]]; then
  fail "bare --label should report the flag by name; got rc=$RC :: $OUT"
else
  ok
fi

# ----- --dry-run needs no agent id -------------------------------------------
# SKILL.md and usage() both promise this. The export above (needed to make the
# tripwire reachable) stopped demonstrating it implicitly, and it pins exactly
# the ordering this change moved: the agent-id check must stay BELOW the
# dry-run exit. Asserted explicitly, with the variable unset for this call only.
OUT="$(env -u QONTINUI_AGENT_ID bash "$SCRIPT" --repo "$REPO" --pr "$PRNUM" \
        --label coord:blocked --dry-run 2>&1)"; RC=$?
if [[ $RC -ne 0 ]]; then
  fail "--dry-run must not require QONTINUI_AGENT_ID; got rc=$RC :: $OUT"
else
  ok
fi

# ----- the tripwire: --dry-run sent nothing -----------------------------------
# Assert the shadow actually works first, or "no record" would prove nothing.
if [[ "$(command -v gh)" != "$STUBDIR/gh" ]]; then
  fail "gh stub did not shadow PATH (resolved to $(command -v gh)); the no-send assertion below is vacuous"
else
  ok
fi
if [[ -e "$SENTINEL" ]]; then
  fail "--dry-run invoked gh: $(cat "$SENTINEL")"
else
  ok
fi

# ----- report -----------------------------------------------------------------
if [[ $FAILURES -ne 0 ]]; then
  echo "set-label self-test: $FAILURES failure(s) across $((CHECKS + FAILURES)) assertion(s)" >&2
  exit 1
fi
echo "set-label self-test: $CHECKS assertion(s) classified correctly (ceiling, short-form suggestion, grammar, no-send)"
