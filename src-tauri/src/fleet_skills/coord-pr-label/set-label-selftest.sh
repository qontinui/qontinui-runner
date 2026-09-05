#!/usr/bin/env bash
# Self-test for set-label.sh's pre-send validation.
#
# Runs the REAL script rather than a copy of it. Most cases go through
# `--dry-run`, which stops before gh and coord; one later section deliberately
# does NOT, in order to cover the gh-failure diagnosis. No network and no real
# gh in either: a PATH-shadowing stub serves both phases -- as a pure tripwire
# for the dry-run corpus (which must never reach even that), and as a
# controllable fixture for the section that does.
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
#     the run is asserted to have left no such record;
#   - and, because it sends nothing, an ACCEPTED dry run states the one thing it
#     could not check -- whether the label exists -- rather than letting "is
#     valid" imply it did. Three classes, because the boundary is the KEY and
#     not the presence of a value: an open-valued key names the
#     `gh label create` that would be needed, while a flag label and the
#     closed-enum `merge-strategy` get the caveat without that command -- and
#     none of them ever asserts an absence a dry run has no evidence for;
#   - and, in the one section that deliberately DOES reach gh: that a FAILING
#     `gh pr edit` is diagnosed rather than merely relayed -- the
#     label-not-found shape names `gh label create`, while an unrelated failure
#     and a bare "not found" that never named the label do not (both directions
#     again: an arm proven only on the shape it recognises is indistinguishable
#     from one that appends the same advice to everything); that gh's own
#     stderr survives being answered on EVERY path, success included, since
#     capturing it to answer one failure must not eat it the rest of the time;
#     and that gh's STDOUT still reaches the terminal, which is what makes the
#     `2>&1 1>&3` ordering a tested property instead of a comment.
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

# expect_dry_contains <label> <substring the dry-run report must contain>
# The label must be ACCEPTED: these assert what a PASSING dry run says about
# itself, which is a different property from what a rejection says.
expect_dry_contains() {
  local label="$1" needle="$2"
  run "$label"
  if [[ $RC -ne 0 ]]; then
    fail "expected accept, got rc=$RC for \"$label\" :: $OUT"
  elif [[ "$OUT" != *"$needle"* ]]; then
    fail "dry-run report for \"$label\" lacks \"$needle\" :: $OUT"
  else
    ok
  fi
}

# expect_dry_absent <label> <substring the dry-run report must NOT contain>
expect_dry_absent() {
  local label="$1" needle="$2"
  run "$label"
  if [[ $RC -ne 0 ]]; then
    fail "expected accept, got rc=$RC for \"$label\" :: $OUT"
  elif [[ "$OUT" == *"$needle"* ]]; then
    fail "dry-run report for \"$label\" should not contain \"$needle\" :: $OUT"
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

# ----- an accepted dry run states what it did NOT check -----------------------
# `--dry-run` clears the grammar and the ceiling, which closes cause 2 of
# `'<label>' not found` (over 50 characters, therefore uncreatable). It cannot
# close cause 1 -- that needs a send -- and a bare "is valid" invites the caller
# to assume it did, which is the reassurance the real run then contradicts.
#
# Three classes, because an arm proven only where it fires is indistinguishable
# from one that fires everywhere, and because the boundary is NOT "carries a
# value": an OPEN-valued key gets the `gh label create` line, while a FLAG label
# and a CLOSED-enum one get the caveat without it (pointing at a repo-wide
# mutation for a label somebody creates once is advice nobody needs -- the same
# over-broad-signposting the post-gh arm narrows its match to avoid).
DYN_LABEL="coord:downstream-of=qontinui-dev-notes#167"
expect_dry_contains "$DYN_LABEL" "NOT checked"
expect_dry_contains "$DYN_LABEL" "gh label create \"$DYN_LABEL\" --repo $REPO"
# ...and it must never assert an absence it has no evidence for. A dry run sent
# nothing, so "does not exist" would be a fresh mis-signpost rather than a fix --
# the label may well have been created already. (The needle is spaced; $REPO's
# own `does-not-exist-selftest` is hyphenated and cannot match it by accident.)
expect_dry_absent "$DYN_LABEL" "does not exist"
# A flag label: caveat yes, create-the-label command no.
expect_dry_contains "coord:blocked" "NOT checked"
expect_dry_absent   "coord:blocked" "gh label create"
# `merge-strategy` is the case that makes the arm KEYED rather than a `*=*`
# test, and it is the only one that can catch that regression: it CARRIES A
# VALUE, so the loose form sweeps it in with the dep labels -- and then justifies
# the advice with "unique to the PR pair", a claim about a key this label is not.
# Its value is one of exactly three strings, so it is a repo-wide label somebody
# creates once, exactly the class `coord:blocked` is excluded for.
expect_dry_contains "coord:merge-strategy=squash" "NOT checked"
expect_dry_absent   "coord:merge-strategy=squash" "gh label create"
# ...and `requires-tag` IS open-valued, so it keeps the command. Without this the
# arm could shrink to the three dep keys and nothing here would notice.
expect_dry_contains "coord:requires-tag=ts-v*" "gh label create"
# The pre-existing success line must survive beside the note -- it is what a
# caller greps for, and burying it would trade one silent surprise for another.
expect_dry_contains "coord:blocked" "is valid"

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

# ----- a failing gh pr edit is answered, not merely relayed --------------------
# These are the only cases that REACH gh, so they deliberately come after the
# tripwire above: they DO invoke it, and running them earlier would make the
# "--dry-run sent nothing" sentinel fire on their traffic rather than on a
# regression. (Two earlier cases also omit `--dry-run` -- the bare-`--label`
# one, and this line's own ancestor -- but they exit at arg parse, long before
# gh; "reach gh" is the property the ordering argument actually needs.)
#
# A second stub shadows the first (prepended, so it wins) and writes to its own
# sentinel, leaving `$SENTINEL` untouched as the record of the dry-run phase.
# It reads its exit code and its stderr from files, so one stub covers the
# success path as well as the failure ones, and it writes a distinguishable
# line to STDOUT -- which is what pins the `2>&1 1>&3` ordering as tested
# behaviour rather than as a claim in a comment.
#
# `--label coord:blocked` throughout: it clears validation and the ceiling, so
# the only thing between the corpus and gh is the code under test.
STUBDIR2="$(mktemp -d)" || { echo "FAIL: mktemp -d failed for the gh stub" >&2; exit 1; }
SENTINEL2="$STUBDIR2/gh-was-called"
GH_STUB_MSG_FILE="$STUBDIR2/message"
GH_STUB_RC_FILE="$STUBDIR2/rc"
GH_STUB_STDOUT="STDOUT-MARKER https://github.com/o/r/pull/1"
{
  echo '#!/usr/bin/env bash'
  echo 'echo "gh $*" >> "$(dirname "$0")/gh-was-called"'
  echo 'echo "STDOUT-MARKER https://github.com/o/r/pull/1"'
  echo 'cat "$(dirname "$0")/message" >&2'
  echo 'exit "$(cat "$(dirname "$0")/rc")"'
} > "$STUBDIR2/gh"
chmod +x "$STUBDIR2/gh"
PATH="$STUBDIR2:$PATH"
export PATH
cleanup2() { rm -rf "$STUBDIR2"; }
trap 'cleanup; cleanup2' EXIT

# The shadow has to be proven again -- this is a DIFFERENT stub from the one
# asserted above. And unlike there, a broken shadow here is not merely vacuous:
# the cases below run without `--dry-run`, so an unshadowed `gh` would make
# three live calls. `exit 1` rather than `fail`, matching the mktemp guards --
# `fail` only counts and returns, and execution would walk straight into them.
if [[ "$(command -v gh)" != "$STUBDIR2/gh" ]]; then
  echo "FAIL: gh stub did not shadow PATH (resolved to $(command -v gh)); refusing to run the live-call cases" >&2
  exit 1
fi
ok

# run_gh <exit code> <stderr line> -- sets RC and OUT.
run_gh() {
  printf '%s\n' "$2" > "$GH_STUB_MSG_FILE"
  printf '%s\n' "$1" > "$GH_STUB_RC_FILE"
  : > "$SENTINEL2"
  OUT="$(bash "$SCRIPT" --repo "$REPO" --pr "$PRNUM" --label coord:blocked 2>&1)"; RC=$?
}

# Every case asserts this. Without it a regression that exits BEFORE gh leaves
# the negative assertions ("no create-the-label advice") trivially true, and a
# green tick would mean only that a code path was never entered.
expect_gh_reached() {
  if [[ ! -s "$SENTINEL2" ]]; then
    fail "gh stub was never invoked ($1); the assertions for this case are vacuous"
  else
    ok
  fi
}

# --- Case 0: SUCCESS. Pins the properties the failure cases cannot see.
#
# This case captures the script's stdout and stderr SEPARATELY, and that is
# load-bearing rather than tidiness. Every other case merges them with `2>&1`,
# and a merged capture cannot see the `2>&1 1>&3` ordering at all: under the
# reversed spelling gh's stderr leaks straight to the real stdout instead of
# being captured, so the merged text is byte-identical either way. What
# separates them is WHICH stream each line lands on -- gh's stdout on the
# script's stdout, gh's stderr re-emitted on the script's stderr. The reversal
# puts gh's notice on stdout and leaves stderr empty, which is what the third
# assertion below catches.
printf '%s
' "A new release of gh is available: 2.0.0 -> 2.1.0" > "$GH_STUB_MSG_FILE"
printf '%s
' 0 > "$GH_STUB_RC_FILE"
: > "$SENTINEL2"
SPLIT_OUT="$STUBDIR2/stdout"; SPLIT_ERR="$STUBDIR2/stderr"
bash "$SCRIPT" --repo "$REPO" --pr "$PRNUM" --label coord:blocked   > "$SPLIT_OUT" 2> "$SPLIT_ERR"; RC=$?
OUT="$(cat "$SPLIT_OUT" "$SPLIT_ERR")"
expect_gh_reached "success case"
# gh's STDOUT reached the script's stdout, not the capture.
if [[ "$(cat "$SPLIT_OUT")" != *"$GH_STUB_STDOUT"* ]]; then
  fail "gh's stdout did not reach the terminal :: $(cat "$SPLIT_OUT")"
else
  ok
fi
# ...and gh's stderr did NOT come out on stdout. This is the assertion that
# fails under the reversed fd ordering, and nothing else here does.
if [[ "$(cat "$SPLIT_OUT")" == *"A new release of gh is available"* ]]; then
  fail "gh's stderr leaked onto stdout (fd ordering reversed) :: $(cat "$SPLIT_OUT")"
else
  ok
fi
# gh's STDERR survives on the SUCCESS path too. gh really does write here when
# nothing is wrong -- update notices, deprecation and auth-scope warnings -- and
# capturing stderr to answer one failure must not eat those the rest of the time.
if [[ "$(cat "$SPLIT_ERR")" != *"A new release of gh is available"* ]]; then
  fail "gh's stderr was swallowed on the SUCCESS path :: $(cat "$SPLIT_ERR")"
else
  ok
fi
if [[ "$OUT" != *"ok: gh added label"* ]]; then
  fail "success path did not report the label add :: $OUT"
else
  ok
fi
# rc 4 = it got past gh and died at the coord POST (COORD_URL is a dead port),
# which anchors that the run really did take the success branch.
if [[ $RC -ne 4 ]]; then
  fail "expected rc=4 (past gh, coord unreachable), got rc=$RC :: $OUT"
else
  ok
fi

# --- Case 1: the label-not-found shape. The REST route (`gh api -X POST
# .../issues/<n>/labels`) reports this as `Label does not exist` (HTTP 404),
# NOT the old `gh pr edit`-era `'<label>' not found` -- and unlike that old
# shape, set-label.sh no longer just prints the `gh label create` command as
# advice: it CREATES the label and retries the add once
# [policy: do-reversible-mechanical-work]. That is three real `gh` calls (POST
# labels, label create, retry POST labels), so this needs a stub whose
# response can differ by call number -- the single fixed-response stub2 above
# cannot exercise the retry-succeeds path. Only the first call looks like the
# 404; label create and the retry both succeed, which is what proves the
# label actually got created and re-sent rather than merely diagnosed.
STUBDIR3="$(mktemp -d)" || { echo "FAIL: mktemp -d failed for the auto-create stub" >&2; exit 1; }
CALL_LOG="$STUBDIR3/gh-was-called"
CALL_COUNT="$STUBDIR3/call-count"
echo 0 > "$CALL_COUNT"
{
  echo '#!/usr/bin/env bash'
  echo 'DIR="$(dirname "$0")"'
  echo 'N=$(($(cat "$DIR/call-count") + 1)); echo "$N" > "$DIR/call-count"'
  echo 'echo "gh $*" >> "$DIR/gh-was-called"'
  echo 'if [[ "$N" == "1" ]]; then echo "Label does not exist" >&2; exit 1; fi'
  echo 'echo "STDOUT-MARKER https://github.com/o/r/pull/1"'
  echo 'exit 0'
} > "$STUBDIR3/gh"
chmod +x "$STUBDIR3/gh"
PATH="$STUBDIR3:$PATH"
export PATH
cleanup3() { rm -rf "$STUBDIR3"; }
trap 'cleanup; cleanup2; cleanup3' EXIT
if [[ "$(command -v gh)" != "$STUBDIR3/gh" ]]; then
  echo "FAIL: gh stub (auto-create) did not shadow PATH (resolved to $(command -v gh)); refusing to run this case" >&2
  exit 1
fi
ok
OUT="$(bash "$SCRIPT" --repo "$REPO" --pr "$PRNUM" --label coord:blocked 2>&1)"; RC=$?
if [[ ! -s "$CALL_LOG" ]]; then
  fail "gh stub (auto-create) was never invoked; the assertions for this case are vacuous"
else
  ok
fi
if [[ "$OUT" != *"Label does not exist"* ]]; then
  fail "label-not-found: gh's own stderr on the first call was swallowed :: $OUT"
else
  ok
fi
if [[ "$OUT" != *"\"coord:blocked\" does not exist in $REPO yet -- creating it"* ]]; then
  fail "label-not-found did not report the auto-create attempt :: $OUT"
else
  ok
fi
if [[ "$(cat "$CALL_LOG")" != *"gh label create coord:blocked --repo $REPO"* ]]; then
  fail "label-not-found did not invoke gh label create with the right label/repo :: $(cat "$CALL_LOG")"
else
  ok
fi
if [[ $RC -ne 4 ]]; then
  fail "label-not-found: expected rc=4 after a successful create+retry (past gh, coord unreachable), got rc=$RC :: $OUT"
else
  ok
fi
if [[ "$OUT" != *"ok: gh added label \"coord:blocked\" to $REPO#$PRNUM"* ]]; then
  fail "label-not-found: the retried add did not report success :: $OUT"
else
  ok
fi
# Restore the fixed-response stub for the remaining cases -- they each expect
# ONE gh call to fail and stay failed, which stub3's call-numbered script does
# not model.
PATH="$STUBDIR2:${PATH#"$STUBDIR3:"}"
export PATH
if [[ "$(command -v gh)" != "$STUBDIR2/gh" ]]; then
  echo "FAIL: could not restore the fixed-response gh stub after the auto-create case (resolved to $(command -v gh))" >&2
  exit 1
fi
ok

# --- Case 2: an unrelated failure must NOT collect the create-the-label advice.
# Without this, "recognises the shape" is indistinguishable from "appends the
# advice to every failure", which is a fresh mis-signpost of its own.
run_gh 1 "gh: authentication required"
expect_gh_reached "auth case"
if [[ $RC -eq 0 ]]; then
  fail "a failing gh pr edit must not exit 0 (auth case)"
else
  ok
fi
if [[ "$OUT" != *"gh: authentication required"* ]]; then
  fail "gh's auth error was swallowed :: $OUT"
else
  ok
fi
if [[ "$OUT" == *"gh label create"* ]]; then
  fail "create-the-label advice attached to an unrelated gh failure :: $OUT"
else
  ok
fi

# --- Case 3: "not found" WITHOUT the label named. An unresolvable repo or PR
# reads exactly like this; it is the near-miss the match is narrowed against,
# and it pins that the needle is the LABEL, not the two words.
run_gh 1 "could not resolve to a Repository: not found"
expect_gh_reached "unresolvable-repo case"
if [[ $RC -eq 0 ]]; then
  fail "a failing gh pr edit must not exit 0 (unresolvable repo case)"
else
  ok
fi
# Positive half, so this case cannot pass by never reaching the code at all.
if [[ "$OUT" != *"could not resolve to a Repository: not found"* ]]; then
  fail "gh's stderr was swallowed (unresolvable repo case) :: $OUT"
else
  ok
fi
if [[ "$OUT" == *"gh label create"* ]]; then
  fail "create-the-label advice attached to a bare \"not found\" that never named the label :: $OUT"
else
  ok
fi

# ----- report -----------------------------------------------------------------
if [[ $FAILURES -ne 0 ]]; then
  echo "set-label self-test: $FAILURES failure(s) across $((CHECKS + FAILURES)) assertion(s)" >&2
  exit 1
fi
echo "set-label self-test: $CHECKS assertion(s) classified correctly (ceiling, short-form suggestion, grammar, no-send, dry-run existence caveat, gh success + failure diagnosis)"
