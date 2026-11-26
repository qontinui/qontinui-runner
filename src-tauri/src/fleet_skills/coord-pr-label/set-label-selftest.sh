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
#     label-add call (`gh api … issues/<n>/labels`) is diagnosed rather than
#     merely relayed -- the
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
# BELOW the label-add call, this test fails closed instead of adding real
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
# between the corpus and the label-add call.
#
# The stub exits NON-zero so a regressed script stops at
# `error: label add failed` instead of continuing into the coord POST -- which,
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
export COORD_PR_LABEL_READBACK_RETRY_SECS=0   # the read-back retry waits for GitHub, not for a stub
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
# The case that first hit, 2026-08-19 (claude-config#296 -> the plans repo#167).
expect_reject "coord:downstream-of=qontinui/qontinui-claude-config#296" \
  'coord:downstream-of=qontinui-claude-config#296'
expect_reject "coord:downstream-of=qontinui/example-plans-repo#1234" \
  'coord:downstream-of=example-plans-repo#1234'
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
expect_absent "coord:downstream-of=some-other-org/example-plans-repo#1234" \
  "drop the owner"
expect_reject "coord:downstream-of=some-other-org/example-plans-repo#1234" \
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
expect_reject "coord:blocked"                 "retired hold label"
expect_reject "coord:experimental"            "retired hold label"
expect_accept "coord:credibility-override"
# The Tier-8b twin of credibility-override: a bare FLAG, so it must clear the
# validator without "=value" -- and its valued spellings are still malformed.
expect_accept "coord:blast-radius-override"
expect_dry_absent "coord:blast-radius-override" 'parameterised labels need "=value"'
expect_reject "coord:blast-radius-override=1" "unknown coord:* label key"
expect_accept "coord:migrate-repair"
expect_accept "coord:merge-strategy=squash"
expect_accept "coord:requires-tag=ts-v*"
# `clears-red` is GitHub-applied only (coord's provenance gate, like priority):
# every spelling is refused with the error that names the working route.
for _cr in "coord:clears-red=some-repo" "coord:clears-red=acme/some-repo" \
           "coord:clears-red" "coord:clears-red=a/b/c"; do
  expect_reject "$_cr" "coord:clears-red must be set on the PR itself"
  expect_reject "$_cr" "labels[]=coord:clears-red=<repo>"
  expect_absent "$_cr" "unknown coord:* label key"
done
# `destructive-attested` is GitHub-applied only too (coord's
# DESTRUCTIVE_ATTESTED_LABEL_ERR): every spelling, well-formed or not, gets the
# error naming the route -- never the generic fallthroughs.
for _da in "coord:destructive-attested=0123456789ab" "coord:destructive-attested=not-a-digest" \
           "coord:destructive-attested" "coord:destructive-attested="; do
  expect_reject "$_da" "coord:destructive-attested=<digest12> must be set on the PR itself"
  expect_reject "$_da" "labels[]=coord:destructive-attested=<digest12>"
  expect_absent "$_da" "unknown coord:* label key"
  expect_absent "$_da" 'parameterised labels need "=value"'
done
# The arm is exact: a near-miss key is NOT this refusal. Widening it to
# `destructive-attested*` would pass every assertion above.
expect_reject "coord:destructive-attestedx=0123456789ab" "unknown coord:* label key"

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
DYN_LABEL="coord:downstream-of=example-plans-repo#167"
expect_dry_contains "$DYN_LABEL" "NOT checked"
expect_dry_contains "$DYN_LABEL" "gh label create \"$DYN_LABEL\" --repo $REPO"
# ...and it must never assert an absence it has no evidence for. A dry run sent
# nothing, so "does not exist" would be a fresh mis-signpost rather than a fix --
# the label may well have been created already. (The needle is spaced; $REPO's
# own `does-not-exist-selftest` is hyphenated and cannot match it by accident.)
expect_dry_absent "$DYN_LABEL" "does not exist"
# A flag label: caveat yes, create-the-label command no.
expect_dry_contains "coord:credibility-override" "NOT checked"
expect_dry_absent   "coord:credibility-override" "gh label create"
expect_dry_contains "coord:blast-radius-override" "NOT checked"
expect_dry_absent   "coord:blast-radius-override" "gh label create"
# `merge-strategy` is the case that makes the arm KEYED rather than a `*=*`
# test, and it is the only one that can catch that regression: it CARRIES A
# VALUE, so the loose form sweeps it in with the dep labels -- and then justifies
# the advice with "unique to the PR pair", a claim about a key this label is not.
# Its value is one of exactly three strings, so it is a repo-wide label somebody
# creates once, exactly the class `coord:credibility-override` is excluded for.
expect_dry_contains "coord:merge-strategy=squash" "NOT checked"
expect_dry_absent   "coord:merge-strategy=squash" "gh label create"
# ...and `requires-tag` IS open-valued, so it keeps the command. Without this the
# arm could shrink to the three dep keys and nothing here would notice.
expect_dry_contains "coord:requires-tag=ts-v*" "gh label create"
# The pre-existing success line must survive beside the note -- it is what a
# caller greps for, and burying it would trade one silent surprise for another.
expect_dry_contains "coord:credibility-override" "is valid"

# ----- grammar rejections must be unchanged by the length guard ---------------
# Needles are exact: a loose "missing" would also match the length guard's own
# "missing-label problem" line.
expect_reject "coord:stacked-on=nohash"       'stacked-on: missing "#<pr_number>"'
expect_reject "coord:state=open"              "coord-set label"
expect_reject "coord:blocked-by=x"            "coord-set label"
expect_reject "coord:operator-review"         "retired label"
expect_reject "coord:version-bump=1"          "retired label"
# The coord hold is served, so every retired-hold refusal names it and none still
# offers the draft fallback coord's RETIRED_HOLD_LABEL_ERR dropped.
for _rh in "coord:blocked" "coord:experimental" "coord:operator-review" "coord:version-bump=1"; do
  expect_reject "$_rh" "coord_set_pr_hold"
  # A coord hold holds nothing on this repo: auto-merge.yml does not read it.
  expect_reject "$_rh" "auto-merge:hold"
  # Removing the label alone lands nothing: auto-merge.yml is edge-triggered.
  expect_reject "$_rh" "then re-fire the PR's lint"
  expect_absent "$_rh" "gh pr ready"
  expect_absent "$_rh" "once coord serves"
done
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
expect_reject "coord:destructive-attested"              "must be set on the PR itself"
expect_reject "coord:destructive-attested=abcdef123456" "must be set on the PR itself"

# `coord:red-main-fix` is rejected by coord with the GENERIC message -- coord
# has no bespoke arm for it, so this mirror must not invent one. This case pins
# the ABSENCE of a bespoke arm: if someone adds one here without adding it to
# `labels_routes.rs` first, the mirror has drifted and this fails. The doctrine
# (why the label buys nothing, and to set it over the REST labels route) lives in
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
        --label coord:credibility-override --dry-run 2>&1)"; RC=$?
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
# `--label coord:credibility-override` throughout: it clears validation and the ceiling, so
# the only thing between the corpus and gh is the code under test.
STUBDIR2="$(mktemp -d)" || { echo "FAIL: mktemp -d failed for the gh stub" >&2; exit 1; }
SENTINEL2="$STUBDIR2/gh-was-called"
GH_STUB_MSG_FILE="$STUBDIR2/message"
GH_STUB_RC_FILE="$STUBDIR2/rc"
GH_STUB_STDOUT="STDOUT-MARKER https://github.com/o/r/pull/1"
cat > "$STUBDIR2/gh" <<'GHSTUB'
#!/usr/bin/env bash
# The label ADD (`-X POST`) and `gh label create` get the fixed response from
# the `message`/`rc` files. A successful add records the label in `added`, and
# the READS model GitHub from fixture files (all optional):
#   labels_rc      a failing labels read (the GitHub read-back UNKNOWN arm)
#   strip_after K  labels reads after the K-th list nothing (coord's strip)
#   pull.N / pull  the N-th pulls read / any pulls read: `state|merged_at|landed`
#   pull_rc        a failing pulls read
#   events         `id<TAB>created_at<TAB>labeled|unlabeled<TAB>actor<TAB>label` lines
#                  read after the POST; events_pre / labels_pre are what the
#                  pre-add mark reads BEFORE it (events_pre_fail fails it); events_rc
#   labels_list    the label names GitHub lists, verbatim (else `added`)
D="$(dirname "$0")"
echo "gh $*" >> "$D/gh-was-called"
bump() { local n; n=$(( $(cat "$D/$1" 2>/dev/null || echo 0) + 1 )); echo "$n" > "$D/$1"; echo "$n"; }
case "$*" in
  *"-X POST"*|"label create"*) ;;
  *"/pulls/"*)
    n="$(bump pull-reads)"
    if [ -r "$D/pull_rc" ]; then exit "$(cat "$D/pull_rc")"; fi
    if [ -r "$D/pull.$n" ]; then cat "$D/pull.$n"; elif [ -r "$D/pull" ]; then cat "$D/pull"; else echo "open||false"; fi
    exit 0 ;;
  *"/events"*)
    # Before the first successful POST (no `posted` marker) this is the
    # pre-add mark: it sees `events_pre`; after it, `events`. events_rc fails
    # both; events_rc_post fails only the reads after the POST.
    if [ -r "$D/events_rc" ]; then exit "$(cat "$D/events_rc")"; fi
    if [ -e "$D/posted" ] && [ -r "$D/events_rc_post" ]; then exit 1; fi
    if [ ! -e "$D/posted" ]; then
      if [ -r "$D/events_pre_fail" ]; then exit 1; fi
      cat "$D/events_pre" 2>/dev/null; exit 0
    fi
    cat "$D/events" 2>/dev/null; exit 0 ;;
  *"/labels"*)
    if [ -r "$D/labels_rc" ]; then exit "$(cat "$D/labels_rc")"; fi
    # Before the first successful POST this is the pre-add read: GitHub lists
    # only what was already there (labels_pre), and the read is not counted.
    if [ ! -e "$D/posted" ]; then
      if [ -r "$D/labels_pre_fail" ]; then exit 1; fi
      cat "$D/labels_pre" 2>/dev/null; exit 0
    fi
    n="$(bump label-reads)"
    if [ -r "$D/labels_rc_after_1" ] && [ "$n" -gt 1 ]; then exit 1; fi
    if [ -r "$D/first_labels_empty" ] && [ "$n" -eq 1 ]; then exit 0; fi
    if [ -r "$D/strip_after" ] && [ "$n" -gt "$(cat "$D/strip_after")" ]; then exit 0; fi
    # GitHub lists what was there before the POST plus what it added.
    # (case-insensitively, as GitHub dedupes label names)
    if [ -r "$D/labels_list" ]; then cat "$D/labels_list"; else cat "$D/labels_pre" "$D/added" 2>/dev/null | sort -uf; fi; exit 0 ;;
esac
echo "STDOUT-MARKER https://github.com/o/r/pull/1"
cat "$D/message" >&2
rc="$(cat "$D/rc")"
if [ "$rc" = 0 ]; then
  for a in "$@"; do case "$a" in "labels[]="*) printf '%s\n' "${a#*=}" >> "$D/added" ;; esac; done
  : > "$D/posted"
fi
exit "$rc"
GHSTUB
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
  rm -f "$STUBDIR2/posted" "$STUBDIR2/added" "$STUBDIR2/label-reads"
  printf '%s\n' "$2" > "$GH_STUB_MSG_FILE"
  printf '%s\n' "$1" > "$GH_STUB_RC_FILE"
  : > "$SENTINEL2"
  OUT="$(bash "$SCRIPT" --repo "$REPO" --pr "$PRNUM" --label coord:credibility-override 2>&1)"; RC=$?
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
bash "$SCRIPT" --repo "$REPO" --pr "$PRNUM" --label coord:credibility-override   > "$SPLIT_OUT" 2> "$SPLIT_ERR"; RC=$?
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
  # The GitHub reads (the pre-add events mark, the label READ-BACK after the
  # retried add) are answered before the call count: GitHub carries the label.
  # The label is on the PR only once the retried POST (call 3) has run.
  echo 'case "$*" in *"-X POST"*|"label create"*) ;; *"/events"*) exit 0 ;; *"/labels"*) [ "$(cat "$DIR/call-count")" -ge 3 ] && echo coord:credibility-override; exit 0 ;; esac'
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
OUT="$(bash "$SCRIPT" --repo "$REPO" --pr "$PRNUM" --label coord:credibility-override 2>&1)"; RC=$?
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
if [[ "$OUT" != *"\"coord:credibility-override\" does not exist in $REPO yet -- creating it"* ]]; then
  fail "label-not-found did not report the auto-create attempt :: $OUT"
else
  ok
fi
if [[ "$(cat "$CALL_LOG")" != *"gh label create coord:credibility-override --repo $REPO"* ]]; then
  fail "label-not-found did not invoke gh label create with the right label/repo :: $(cat "$CALL_LOG")"
else
  ok
fi
if [[ $RC -ne 4 ]]; then
  fail "label-not-found: expected rc=4 after a successful create+retry (past gh, coord unreachable), got rc=$RC :: $OUT"
else
  ok
fi
if [[ "$OUT" != *"ok: gh added label \"coord:credibility-override\" to $REPO#$PRNUM"* ]]; then
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

# ----- the coord write lands only under a tenant PROVEN to own the repo --------
# The multi-tenant-device defect (2026-09-23; same class as
# qontinui-claude-config#1104): coord writes the row under QONTINUI_AGENT_ID's
# worktree-row tenant, which on a device bound to several tenants is often the
# wrong one. Step 2 probes that tenant with a no-op POST (`labels: []`), proves
# it owns the repo through the author-session door under a token CLAIMING it,
# and only then writes. These cases need a controllable coord, so a `curl` stub
# is prepended to PATH -- after every case above, which rely on the real curl
# failing against the dead COORD_URL port.
#
# The stub serves five calls by URL (and, for /pr-merge/labels, by whether the
# payload is the empty probe), logs each as one line, writes the canned body to
# the -o file, and prints the canned status for -w. The fifth is the read-back
# (`GET /coord/agent-pr-labels`), whose query arrives as `-G --data-urlencode`
# pairs; they are logged after the URL so a case can assert what was asked.
STUBDIR3="$(mktemp -d)" || { echo "FAIL: mktemp -d failed for the curl stub" >&2; exit 1; }
cleanup3() { rm -rf "$STUBDIR3"; }
trap 'cleanup; cleanup2; cleanup3' EXIT
{
  echo '#!/usr/bin/env bash'
  echo 'd="$(dirname "$0")"; out=""; data=""; url=""; hdr=""; q=""; getflag=""'
  echo 'while [ $# -gt 0 ]; do case "$1" in'
  echo '  -o) out="$2"; shift 2 ;; -d) data="$2"; shift 2 ;; -H) hdr="$hdr|$2"; shift 2 ;;'
  echo '  --data-urlencode) q="$q&$2"; shift 2 ;; -G) getflag=1; shift ;;'
  echo '  -w|-X|-m|--connect-timeout) shift 2 ;; -*) shift ;; *) url="$1"; shift ;; esac; done'
  echo 'case "$url" in'
  echo '  */agents/credential) k=mint ;;'
  echo '  */author-session) k=door ;;'
  echo '  */coord/agent-pr-labels) if [ -n "$getflag" ]; then k=read; else k=read_not_get; fi ;;'
  echo '  */pr-merge/labels) case "$data" in *"\"labels\": []"*) k=probe ;; *) k=post ;; esac ;;'
  echo '  */ui-bridge/invoke/get_coord_device_token) k=runner ;;'
  echo '  *) k=other ;; esac'
  echo 'bearer=""; case "$hdr" in *"@"*) f="${hdr##*@}"; [ -r "$f" ] && bearer="$(cat "$f")" ;; esac'
  # A runner-minted token (signature `.runner`) gets the read_runner answer when
  # one is staged -- coord refusing the runner's token on the read-back only.
  echo 'case "$k:$bearer" in read:*.runner) [ -r "$d/read_runner.code" ] && k=read_runner ;; esac'
  echo 'printf "%s %s%s %s\n" "$k" "$url" "$q" "$data" >> "$d/calls"'
  echo '[ -n "$bearer" ] && printf "%s\n" "$bearer" >> "$d/bearers"'
  echo '[ -n "$out" ] && cat "$d/$k.body" > "$out" 2>/dev/null'
  echo 'cat "$d/$k.code"'
} > "$STUBDIR3/curl"
chmod +x "$STUBDIR3/curl"
PATH="$STUBDIR3:$PATH"
export PATH
if [[ "$(command -v curl)" != "$STUBDIR3/curl" ]]; then
  echo "FAIL: curl stub did not shadow PATH; refusing to run the coord-reaching cases" >&2
  exit 1
fi
ok

T_OWN="c231d9da-0000-4000-8000-000000000001"
T_WRONG="b3ecb579-0000-4000-8000-000000000002"
# mkjwt <tenant> [exp-offset-seconds] -> an UNSIGNED header.payload.sig; the
# script only ever reads the payload, and coord is the stub.
mkjwt() {
  H_T="$1" H_OFF="${2:-3600}" python3 -c 'import base64,json,os,time
b=lambda o: base64.urlsafe_b64encode(json.dumps(o).encode()).decode().rstrip("=")
print(b({"alg":"EdDSA"})+"."+b({"tenant_id":os.environ["H_T"],"exp":int(time.time())+int(os.environ["H_OFF"])})+".sig")'
}
# Isolate the credential cascade from this box: no env token, an empty HOME
# (no file token, no machine.json), a pinned device id, and no local-runner
# rung (CTC_NO_RUNNER=1: the box's runner is not part of this fixture, and its
# 127.0.0.1 mint is not a coord call, which T13 counts).
FAKEHOME="$STUBDIR3/home"; mkdir -p "$FAKEHOME"
unset COORD_DEVICE_JWT
export CTC_NO_RUNNER=1
export QONTINUI_MACHINE_ID="11111111-2222-4333-8444-555555555555"
printf '0\n' > "$GH_STUB_RC_FILE"; : > "$GH_STUB_MSG_FILE"

# stub <kind> <code> [body]
stub() { printf '%s\n' "$2" > "$STUBDIR3/$1.code"; printf '%s' "${3:-}" > "$STUBDIR3/$1.body"; }
# run_coord <label> -> RC, OUT; resets the call log.
run_coord() {
  : > "$STUBDIR3/calls"; : > "$STUBDIR3/bearers"
  # Every run starts before its own POST: no `posted`, nothing `added`.
  rm -f "$STUBDIR2/posted" "$STUBDIR2/added" "$STUBDIR2/label-reads"
  OUT="$(HOME="$FAKEHOME" bash "$SCRIPT" --repo "$REPO" --pr 7 --label "$1" 2>&1)"; RC=$?
}
calls_of() { grep -c "^$1 " "$STUBDIR3/calls" 2>/dev/null || true; }
expect_rc() { if [[ $RC -ne $1 ]]; then fail "$2: expected rc=$1, got rc=$RC :: $OUT"; else ok; fi; }
expect_out() { if [[ "$OUT" != *"$1"* ]]; then fail "$2: output lacks \"$1\" :: $OUT"; else ok; fi; }
expect_calls() { local n; n="$(calls_of "$1")"; if [[ "$n" != "$2" ]]; then fail "$3: expected $2 '$1' call(s), got $n :: $(cat "$STUBDIR3/calls")"; else ok; fi; }

OK_POST="{\"tenant_id\":\"$T_OWN\",\"repo\":\"$REPO\",\"pr_number\":7,\"written\":1,\"deleted\":0,\"rejected\":[]}"
# rb_body <name>... -> a GET /coord/agent-pr-labels 200 body listing those labels.
rb_body() {
  H_REPO="$REPO" python3 -c 'import json,os,sys
rows=[{"name":n,"source":"coord_skill","added_at":"2026-09-23T00:00:00Z"} for n in sys.argv[1:]]
print(json.dumps({"repo":os.environ["H_REPO"],"pr_number":7,"labels":rows,"count":len(rows)}))' "$@"
}
# The read-back answers "present" unless a case says otherwise.
stub read 200 "$(rb_body "coord:stacked-on=#6")"

# --- T1: PROVEN. The probe names T_OWN, the mint returns a T_OWN token, the
# door answers 200 -> the real write goes out and the ok line names the tenant.
stub probe 200 "{\"tenant_id\":\"$T_OWN\",\"written\":0,\"deleted\":0,\"rejected\":[]}"
stub mint 200 "{\"token\":\"$(mkjwt "$T_OWN")\"}"
stub door 200 "{\"repo\":\"$REPO\",\"pr\":7,\"resolved\":false}"
stub post 200 "$OK_POST"
run_coord "coord:stacked-on=#6"
expect_rc 0 "T1 proven"
expect_out "owner check: proven: tenant $T_OWN owns $REPO" "T1 proven"
expect_out "coord recorded label" "T1 proven"
expect_out "(read back: present, source=coord_skill; on GitHub: present)" "T1 proven"
expect_calls probe 1 "T1"; expect_calls door 1 "T1"; expect_calls post 1 "T1"
expect_calls read 1 "T1 (read-back)"
# The read asked about THIS PR as a GET (`-G`: without it curl would POST the
# query as a body and coord would answer 405 -- the stub files that call as
# `read_not_get`, so this assertion fails), url-encoded, under a bearer (two staged bearers:
# the owner check's and the read-back's).
if ! grep -q "^read http://127.0.0.1:1/coord/agent-pr-labels&repo=$REPO&pr_number=7 " "$STUBDIR3/calls"; then fail "T1: read-back did not ask repo=$REPO pr_number=7 :: $(cat "$STUBDIR3/calls")"; else ok; fi
if [[ "$(grep -c '^Authorization: Bearer ' "$STUBDIR3/bearers")" != 2 ]]; then fail "T1: expected 2 bearer-carrying calls (door + read) :: $(cat "$STUBDIR3/bearers")"; else ok; fi
# The minted token is REUSED for the read-back, not minted twice.
expect_calls mint 1 "T1 (read-back reuses the minted token)"
# The mint NAMED the tenant: a tenant-less mint is the defect's own shape.
if ! grep -q "^mint .*\"tenant_id\": \"$T_OWN\"" "$STUBDIR3/calls"; then fail "T1: the mint did not send tenant_id=$T_OWN :: $(cat "$STUBDIR3/calls")"; else ok; fi
# The probe really was the empty, merge-mode, no-op write.
if ! grep -q '^probe .*"labels": \[\], "mode": "merge"' "$STUBDIR3/calls"; then fail "T1: probe was not labels=[] mode=merge :: $(cat "$STUBDIR3/calls")"; else ok; fi

# --- T2: REFUTED -- the defect itself. The agent row carries T_WRONG, whose
# token the door answers 404: the row must be WITHHELD, rc 5, and the real
# write must never go out.
stub probe 200 "{\"tenant_id\":\"$T_WRONG\",\"written\":0,\"deleted\":0,\"rejected\":[]}"
stub mint 200 "{\"token\":\"$(mkjwt "$T_WRONG")\"}"
stub door 404 '{"error":"not found"}'
run_coord "coord:upstream-of=qontinui-runner#1705"
expect_rc 5 "T2 refuted"
expect_out "WITHHELD" "T2 refuted"
expect_out "does NOT own $REPO" "T2 refuted"
expect_out "the GitHub label read back present at step 1" "T2 refuted"
expect_calls post 0 "T2 (no wrong-tenant row)"
expect_calls read 0 "T2 (withheld: nothing written, nothing to read back)"

# --- T3: the mint ignores tenant_id and hands back ANOTHER tenant's token (a
# coord predating the field). Using it would ask the door about the wrong
# tenant, so it is rejected: UNKNOWN, rc 5, no door call, no write.
stub probe 200 "{\"tenant_id\":\"$T_OWN\",\"written\":0,\"deleted\":0,\"rejected\":[]}"
stub mint 200 "{\"token\":\"$(mkjwt "$T_WRONG")\"}"
run_coord "coord:stacked-on=#6"
expect_rc 5 "T3 wrong-claim mint"
expect_out "UNKNOWN" "T3 wrong-claim mint"
expect_out "claiming tenant $T_WRONG" "T3 wrong-claim mint"
expect_calls door 0 "T3"; expect_calls post 0 "T3"

# --- T4: the door fails (5xx) -- not a proof either way: UNKNOWN, withheld.
stub mint 200 "{\"token\":\"$(mkjwt "$T_OWN")\"}"
stub door 503 '{"error":"unavailable"}'
run_coord "coord:stacked-on=#6"
expect_rc 5 "T4 door 503"
expect_out "UNKNOWN: the author-session door answered HTTP 503" "T4 door 503"
expect_calls post 0 "T4"

# --- T5: a static env token is used ONLY when it claims the write tenant. One
# claiming another tenant is skipped for the mint; one claiming the write
# tenant is used and no mint happens.
stub door 200 "{\"repo\":\"$REPO\",\"pr\":7,\"resolved\":false}"
COORD_DEVICE_JWT="$(mkjwt "$T_WRONG")" run_coord "coord:stacked-on=#6"
expect_rc 0 "T5a env token for another tenant"
expect_calls mint 1 "T5a (env token skipped, mint used)"
COORD_DEVICE_JWT="$(mkjwt "$T_OWN")" run_coord "coord:stacked-on=#6"
expect_rc 0 "T5b env token for the write tenant"
expect_calls mint 0 "T5b (env token used)"
expect_out "bearer=env" "T5b"
# ...and an EXPIRED one for the right tenant is not used.
COORD_DEVICE_JWT="$(mkjwt "$T_OWN" -30)" run_coord "coord:stacked-on=#6"
expect_calls mint 1 "T5c (expired env token skipped)"

# --- T6: coord's enforce arm re-tenanted the write itself -- the probe echoes
# no tenant_id. coord made the ownership decision; no door call is needed.
# With the WRITE echoing no tenant either, no credential can be scoped to the
# read-back, so the answer is UNKNOWN (rc 6) -- never a guessed tenant, never ok.
stub probe 200 '{"written":0,"deleted":0,"rejected":[]}'
stub post 200 '{"written":1,"deleted":0,"rejected":[]}'
run_coord "coord:stacked-on=#6"
expect_rc 6 "T6 enforce re-tenanted, no tenant echoed"
expect_out "coord derived the tenant from repo ownership itself" "T6"
expect_out "written but read-back UNKNOWN" "T6"
expect_calls door 0 "T6"; expect_calls post 1 "T6"; expect_calls read 0 "T6"
if [[ "$OUT" == *"ok: coord recorded"* ]]; then fail "T6: UNKNOWN read-back printed ok :: $OUT"; else ok; fi
# ...and when the WRITE does echo its tenant, the read-back is keyed on it.
stub post 200 "$OK_POST"
run_coord "coord:stacked-on=#6"
expect_rc 0 "T6b enforce re-tenanted, write echoes tenant"
expect_calls read 1 "T6b"

# --- T7: coord's enforce arm refuses (422 repo_not_owned_by_tenant) at the
# probe -> rc 5, no write attempted.
stub probe 422 '{"error":"repo_not_owned_by_tenant","repo":"x","bound_owner_count":0}'
run_coord "coord:stacked-on=#6"
expect_rc 5 "T7 enforce refusal"
expect_calls post 0 "T7"

# --- T8: no device id and no usable token -> UNKNOWN, withheld (never a
# bearerless request, whose 401 would say nothing about the tenant).
stub probe 200 "{\"tenant_id\":\"$T_OWN\",\"written\":0,\"deleted\":0,\"rejected\":[]}"
( unset QONTINUI_MACHINE_ID; run_coord "coord:stacked-on=#6"; printf '%s\n%s\n' "$RC" "$OUT" > "$STUBDIR3/t8" )
RC="$(head -1 "$STUBDIR3/t8")"; OUT="$(tail -n +2 "$STUBDIR3/t8")"
expect_rc 5 "T8 no device id"
expect_out "no device_id" "T8"
expect_calls door 0 "T8"; expect_calls mint 0 "T8"

# --- T9: the write lands under a DIFFERENT tenant than the probe proved (the
# agent row moved between the calls) -> not an unqualified ok.
stub probe 200 "{\"tenant_id\":\"$T_OWN\",\"written\":0,\"deleted\":0,\"rejected\":[]}"
stub post 200 "{\"tenant_id\":\"$T_WRONG\",\"written\":1,\"deleted\":0,\"rejected\":[]}"
run_coord "coord:stacked-on=#6"
expect_rc 4 "T9 tenant moved"
expect_out "not the proven $T_OWN" "T9"
expect_calls read 0 "T9 (no read-back after a tenant mismatch)"

# --- T10 (review r1, HIGH): a 2xx probe that is NOT a label-set response --
# empty, non-JSON, or a null tenant_id -- is UNKNOWN, never "enforce
# re-tenanted". Only a well-formed body with the key ABSENT is that (T6).
for body in "" "<html>captive portal</html>" "{\"tenant_id\":null,\"written\":0,\"rejected\":[]}"; do
  stub probe 200 "$body"
  stub post 200 "$OK_POST"
  run_coord "coord:stacked-on=#6"
  expect_rc 5 "T10 malformed probe body [$body]"
  expect_calls post 0 "T10 [$body]"; expect_calls door 0 "T10 [$body]"
done

# --- T11 (review r1): an expired MINTED token for the right tenant is not used.
stub probe 200 "{\"tenant_id\":\"$T_OWN\",\"written\":0,\"deleted\":0,\"rejected\":[]}"
stub mint 200 "{\"token\":\"$(mkjwt "$T_OWN" -30)\"}"
stub door 200 "{\"repo\":\"$REPO\",\"pr\":7,\"resolved\":false}"
run_coord "coord:stacked-on=#6"
expect_rc 5 "T11 expired mint"
expect_out "expired" "T11"
expect_calls door 0 "T11"; expect_calls post 0 "T11"

# --- T12 (review r1): no device credential is sent to a plain-http, non-loopback
# COORD_URL -- the check withholds instead.
stub mint 200 "{\"token\":\"$(mkjwt "$T_OWN")\"}"
COORD_URL="http://coord.example.test" run_coord "coord:stacked-on=#6"
expect_rc 5 "T12 non-https coord url"
expect_out "neither https nor loopback" "T12"
expect_calls mint 0 "T12"; expect_calls door 0 "T12"
COORD_URL="http://localhost:x@coord.example.test/" run_coord "coord:stacked-on=#6"
expect_rc 5 "T12b userinfo-retargeted loopback url"
expect_calls mint 0 "T12b"

# --- T13: a ws:// / wss:// COORD_URL is coord's WEBSOCKET door (the runner
# exports COORD_URL=wss://coord.qontinui.io/ws into every session it spawns). It
# is skipped, COORD_HTTP_URL is used, and no call is aimed at the websocket URL.

# ----- the write is READ BACK, and only a present row is ok -------------------
# Plan 2026-09-10-coord-pr-labels-have-no-agent-read-door, Phase 3. Three
# outcomes, three exits: present -> 0, a 200 WITHOUT the row -> 4 (coord
# contradicted itself), anything that is not an answer -> 6 (UNKNOWN). The
# absent and unknown arms are asserted to never print `ok: coord recorded`,
# which is the line a caller greps for.
stub probe 200 "{\"tenant_id\":\"$T_OWN\",\"written\":0,\"deleted\":0,\"rejected\":[]}"
stub mint 200 "{\"token\":\"$(mkjwt "$T_OWN")\"}"
stub door 200 "{\"repo\":\"$REPO\",\"pr\":7,\"resolved\":false}"
stub post 200 "$OK_POST"
COORD_URL="wss://coord.example.test/ws" COORD_HTTP_URL="https://coord.example.test" run_coord "coord:stacked-on=#6"
expect_rc 0 "T13 websocket COORD_URL falls through to COORD_HTTP_URL"
expect_out "coord recorded label" "T13"
expect_calls post 1 "T13"
if grep -q "wss://" "$STUBDIR3/calls"; then fail "T13: a call went to the websocket URL :: $(cat "$STUBDIR3/calls")"; else ok; fi
if [[ "$(grep -c " https://coord.example.test/" "$STUBDIR3/calls")" -ne "$(wc -l < "$STUBDIR3/calls" | tr -d ' ')" ]]; then
  fail "T13: not every call used COORD_HTTP_URL :: $(cat "$STUBDIR3/calls")"; else ok; fi
# T13b: an upper-case scheme is still the websocket door, and with no
# COORD_HTTP_URL the precedence ends at the hosted base.
( unset COORD_HTTP_URL; COORD_URL="WSS://coord.example.test/ws" run_coord "coord:stacked-on=#6"
  printf '%s\n' "$RC" > "$STUBDIR3/t13b.rc" )
if [[ "$(cat "$STUBDIR3/t13b.rc")" != 0 ]]; then fail "T13b: expected rc=0, got $(cat "$STUBDIR3/t13b.rc") :: $(cat "$STUBDIR3/calls")"; else ok; fi
if [[ "$(grep -c " https://coord.qontinui.io/" "$STUBDIR3/calls")" -ne "$(wc -l < "$STUBDIR3/calls" | tr -d ' ')" ]]; then
  fail "T13b: not every call used the hosted base :: $(cat "$STUBDIR3/calls")"; else ok; fi
# T13c: the read-back under the local runner's token answers 401 -- the runner
# token is dropped and the read is re-staged ONCE, reaching the mint, exactly as
# the library's own walkers retry it. Production coord, so the runner is asked.
RUNNER_TOK="$(mkjwt "$T_OWN" | sed 's/\.sig$/.runner/')"
stub runner 200 "{\"success\":true,\"data\":\"$RUNNER_TOK\"}"
stub read_runner 401 '{"error":"unknown signer"}'
stub read 200 "$(rb_body "coord:stacked-on=#6")"
COORD_URL="https://coord.qontinui.io" CTC_NO_RUNNER=0 run_coord "coord:stacked-on=#6"
expect_rc 0 "T13c a 401 to the runner's token on the read-back re-stages to the mint"
expect_out "coord recorded label" "T13c"
expect_calls runner 1 "T13c (the owner check staged the runner's token)"
expect_calls read_runner 1 "T13c (the first read went out under it)"
expect_calls read 1 "T13c (the retry read went out under the mint)"
expect_calls mint 1 "T13c (the re-stage minted)"
rm -f "$STUBDIR3/read_runner.code" "$STUBDIR3/read_runner.body" "$STUBDIR3/runner.code" "$STUBDIR3/runner.body"

no_ok() { if [[ "$OUT" == *"ok: coord recorded"* ]]; then fail "$1: printed ok without a present read-back :: $OUT"; else ok; fi; }

# --- R1: 200 listing OTHER labels only -> a contradiction, rc 4, both bodies shown.
stub read 200 "$(rb_body "coord:merge-strategy=squash")"
run_coord "coord:stacked-on=#6"
expect_rc 4 "R1 read-back absent"
expect_out "CONTRADICTION" "R1"
expect_out "coord said written=1" "R1"
expect_out "coord:merge-strategy=squash" "R1 (read body shown)"
expect_calls read 1 "R1"; no_ok "R1"
# An EMPTY list is the same contradiction, not an UNKNOWN.
stub read 200 "$(rb_body)"
run_coord "coord:stacked-on=#6"
expect_rc 4 "R1b read-back empty list"

# --- R2: 404 -- including a coord that predates the door -> UNKNOWN, rc 6.
stub read 404 '{"error":"not found"}'
run_coord "coord:stacked-on=#6"
expect_rc 6 "R2 read door 404"
expect_out "written but read-back UNKNOWN" "R2"
expect_out "PREDATES GET /coord/agent-pr-labels" "R2"
no_ok "R2"

# --- R3: unreachable (000), 401, 5xx and an unparseable 200 are all UNKNOWN.
for spec in "000|" "401|{\"error\":\"unauthorized\"}" "503|{\"error\":\"unavailable\"}" "200|<html>captive portal</html>" "200|{\"repo\":\"x\",\"pr_number\":7}" "200|{\"repo\":\"x\",\"pr_number\":8,\"labels\":[{\"name\":\"coord:stacked-on=#6\"}]}"; do
  stub read "${spec%%|*}" "${spec#*|}"
  run_coord "coord:stacked-on=#6"
  expect_rc 6 "R3 read-back [$spec]"
  expect_out "written but read-back UNKNOWN" "R3 [$spec]"
  no_ok "R3 [$spec]"
done

# --- R4: a bare-repo dep label is STORED canonicalized (owner/repo), so that
# row counts as the label -- and the ok line names the stored spelling. A row
# for a different PR number is not it (rc 4), so the match is not "any prefix".
stub read 200 "$(rb_body "coord:downstream-of=qontinui/qontinui-web#9")"
run_coord "coord:downstream-of=qontinui-web#9"
expect_rc 0 "R4 canonical form"
expect_out 'stored as "coord:downstream-of=qontinui/qontinui-web#9"' "R4"
stub read 200 "$(rb_body "coord:downstream-of=qontinui/qontinui-web#90")"
run_coord "coord:downstream-of=qontinui-web#9"
expect_rc 4 "R4b near-miss PR number"

# --- R5: the dry run is unchanged -- no read-back, no coord call at all.
: > "$STUBDIR3/calls"
OUT="$(HOME="$FAKEHOME" bash "$SCRIPT" --repo "$REPO" --pr 7 --label "coord:stacked-on=#6" --dry-run 2>&1)"; RC=$?
expect_rc 0 "R5 dry run"
if [[ -s "$STUBDIR3/calls" ]]; then fail "R5: --dry-run reached coord :: $(cat "$STUBDIR3/calls")"; else ok; fi

# --- T15: a 200 from the ownership door about ANOTHER repo is not a proof (the
# shared owner-tenant library checks the body names the repo asked about).
stub probe 200 "{\"tenant_id\":\"$T_OWN\",\"written\":0,\"deleted\":0,\"rejected\":[]}"
stub mint 200 "{\"token\":\"$(mkjwt "$T_OWN")\"}"
stub door 200 '{"repo":"qontinui/some-other-repo","pr":7,"resolved":false}'
run_coord "coord:stacked-on=#6"
expect_rc 5 "T15 door 200 about another repo"
expect_calls post 0 "T15"
expect_out "DIFFERENT repo" "T15 (the about-another-repo arm, not the generic one)"
# The proof is repo-level: the door is asked at pr 0, never at the label's PR.
if grep -q "/author-session" "$STUBDIR3/calls" && ! grep "/author-session" "$STUBDIR3/calls" | grep -qv "/0/author-session"; then ok
else fail "T15b: the ownership door was not asked at pr 0 :: $(grep author-session "$STUBDIR3/calls")"; fi

# --- T16: the vendored library is byte-identical to scripts/lib's. It ships in
# the runner's fleet_skills bundle, where scripts/lib/ does not exist, so a copy
# that drifted would run a different proof there than here.
_lib_here="$HERE/lib/coord-tenant-credential.sh"; _lib_src="$HERE/../../../scripts/lib/coord-tenant-credential.sh"
if [[ ! -r "$_lib_here" ]]; then fail "T16: $_lib_here is missing (the bundled skill would have no owner-tenant proof)"
elif [[ ! -r "$_lib_src" ]]; then ok   # inside the runner bundle: scripts/lib/ does not exist there, and fleet-skills parity pins the bytes
elif cmp -s "$_lib_here" "$_lib_src"; then ok
else fail "T16: lib/coord-tenant-credential.sh differs from scripts/lib/coord-tenant-credential.sh -- copy scripts/lib's forward"; fi

# --- T17: a STALE library (readable, but predating ctc_prove_tenant) withholds
# with exit 5 and says why -- the CTC_OK=0 arm, which nothing else reaches. The
# load line is a `&&` chain under `set -e`; it survives a failing `declare -F`
# only because that is not the chain's LAST command, so this pins that too.
_t17="$(mktemp -d)"; mkdir -p "$_t17/lib"
cp "$SCRIPT" "$_t17/set-label.sh"
printf '%s\n' '# a library copy that predates ctc_prove_tenant' 'ctc_is_uuid() { return 0; }' > "$_t17/lib/coord-tenant-credential.sh"
stub probe 200 "{\"tenant_id\":\"$T_OWN\",\"written\":0,\"deleted\":0,\"rejected\":[]}"
: > "$STUBDIR3/calls"; : > "$STUBDIR3/bearers"
OUT="$(HOME="$FAKEHOME" bash "$_t17/set-label.sh" --repo "$REPO" --pr 7 --label "coord:stacked-on=#6" 2>&1)"; RC=$?  # skill-self-path-ok: a copy under mktemp -d beside a deliberately stale lib/, not this skill's own files
expect_rc 5 "T17 stale library withholds"
expect_out "is not usable" "T17"
expect_calls post 0 "T17"
rm -rf "$_t17"

# ----- no success claim rests on gh's exit code: the label is READ from GitHub -
# The 2026-11-17 incident (qontinui/qontinui-coord#4227): `coord:downstream-of=
# qontinui/qontinui-web#2281` was accepted by GitHub, then stripped two seconds
# later by coord because web#2281 had already landed. The script printed
# `ok: gh added label`, and the missing coord row then read as a CONTRADICTION.
# The gh stub above models GitHub's reads (labels, events, the upstream PR), so
# each arm is driven by what GitHub would SAY, not by gh's exit code.
gh_fx_reset() {
  rm -f "$STUBDIR2"/label-reads "$STUBDIR2"/pull-reads "$STUBDIR2"/strip_after "$STUBDIR2"/pull "$STUBDIR2"/pull.* \
    "$STUBDIR2"/pull_rc "$STUBDIR2"/events "$STUBDIR2"/events_pre "$STUBDIR2"/first_labels_empty "$STUBDIR2"/labels_rc_after_1 "$STUBDIR2"/events_rc "$STUBDIR2"/events_pre_fail "$STUBDIR2"/posted "$STUBDIR2"/labels_pre "$STUBDIR2"/labels_pre_fail "$STUBDIR2"/events_rc_post "$STUBDIR2"/labels_rc "$STUBDIR2"/labels_list "$STUBDIR2"/added
  : > "$SENTINEL2"
}
gh_calls() { cat "$SENTINEL2"; }
no_gh_ok() { if [[ "$OUT" == *"ok: gh added label"* || "$OUT" == *"was already on"* ]]; then fail "$1: claimed the label is on the PR :: $OUT"; else ok; fi; }
no_post() { if grep -q -- "-X POST" "$SENTINEL2"; then fail "$1: the label add was sent :: $(gh_calls)"; else ok; fi; }
stub probe 200 "{\"tenant_id\":\"$T_OWN\",\"written\":0,\"deleted\":0,\"rejected\":[]}"
stub mint 200 "{\"token\":\"$(mkjwt "$T_OWN")\"}"
stub door 200 "{\"repo\":\"$REPO\",\"pr\":7,\"resolved\":false}"
stub post 200 "$OK_POST"
DEP="coord:downstream-of=qontinui/qontinui-web#2281"
BARE="coord:downstream-of=qontinui-web#2281"
STRIP_EVENT="$(printf '201\t2099-01-01T00:00:01Z\tlabeled\tjspinak\t%s\n202\t2099-01-01T00:00:03Z\tunlabeled\tqontinui-merge-orchestrator\t%s' "$DEP" "$DEP")"

# --- G0: the ordinary ok names BOTH read-backs, and the upstream was read at
# the right coordinates (a bare repo takes the PR's owner; `#<n>` is this repo).
gh_fx_reset
stub read 200 "$(rb_body "coord:stacked-on=#6")"
run_coord "coord:stacked-on=#6"
expect_rc 0 "G0 ok"
expect_out "(read back on GitHub: present)" "G0 step-1 read-back"
expect_out "source=coord_skill; on GitHub: present)" "G0 final read-back"
if grep -q "repos/$REPO/pulls/6" "$SENTINEL2"; then ok; else fail "G0: stacked-on=#6 did not read repos/$REPO/pulls/6 :: $(gh_calls)"; fi
if [[ "$(grep -c "issues/7/labels --paginate" "$SENTINEL2")" == 3 ]]; then ok; else fail "G0: expected 3 GitHub label reads (pre-add, step 1, final) :: $(gh_calls)"; fi
gh_fx_reset
stub read 200 "$(rb_body "coord:downstream-of=qontinui/qontinui-web#2281")"
run_coord "$BARE"
expect_rc 0 "G0b bare-repo dep label"
expect_out "owner is not stated" "G0b"
if grep -q "/pulls/" "$SENTINEL2"; then fail "G0b: a bare repo's owner was guessed for the pre-flight :: $(gh_calls)"; else ok; fi

# --- G1: the upstream has ALREADY landed -> NOT-SENT before anything is
# sent: no label add, no coord call, and never an ok. Both land shapes count --
# GitHub's merge, and coord's rebase-ff land (closed, merged_at null, coord:landed).
for pull in "closed||true" "closed|2026-11-17T09:25:43Z|true"; do
  gh_fx_reset; printf '%s\n' "$pull" > "$STUBDIR2/pull"
  run_coord "$DEP"
  expect_rc 7 "G1 upstream landed [$pull]"
  expect_out "NOT-SENT" "G1 [$pull]"
  expect_out "Nothing was sent" "G1 [$pull]"
  if [[ "$OUT" == *STRIPPED-STALE* || "$OUT" == *"is not on"* ]]; then fail "G1 [$pull]: claimed a strip / a label state it never read :: $OUT"; else ok; fi
  no_post "G1 [$pull]"; no_gh_ok "G1 [$pull]"; no_ok "G1 [$pull]"
  if [[ -s "$STUBDIR3/calls" ]]; then fail "G1 [$pull]: coord was called :: $(cat "$STUBDIR3/calls")"; else ok; fi
done
# ...but a CLOSED upstream that never landed is not satisfied: the add goes out.
gh_fx_reset; printf 'closed||false\n' > "$STUBDIR2/pull"
run_coord "$DEP"
if [[ $RC -eq 7 ]]; then fail "G1c: a closed-unlanded upstream was called stale :: $OUT"; else ok; fi
if grep -q -- "-X POST" "$SENTINEL2"; then ok; else fail "G1c: the add was not sent :: $(gh_calls)"; fi

# --- G2: the incident, step 1 arm. The upstream reads open at the pre-flight,
# GitHub accepts the add, coord strips it before the read-back. Exit 7, no gh
# ok, and coord is never written (a row coord would delete again).
gh_fx_reset
printf 'open||false\n' > "$STUBDIR2/pull.1"; printf 'closed||true\n' > "$STUBDIR2/pull"
printf '0\n' > "$STUBDIR2/strip_after"; printf '%s\n' "$STRIP_EVENT" > "$STUBDIR2/events"
run_coord "$DEP"
expect_rc 7 "G2 stripped right after the add"
expect_out "removed by qontinui-merge-orchestrator at 2099-01-01T00:00:03Z" "G2"
no_gh_ok "G2"; no_ok "G2"
expect_calls post 0 "G2 (no coord write for a stripped label)"
if [[ "$OUT" == *CONTRADICTION* ]]; then fail "G2: reported as a contradiction :: $OUT"; else ok; fi

# --- G3: the incident, coord arm. GitHub reads present at step 1, then coord
# strips it and drops its own row: coord answers written=1, its read door lists
# nothing. That is the strip, not a CONTRADICTION -- exit 7.
gh_fx_reset
printf 'open||false\n' > "$STUBDIR2/pull.1"; printf 'closed||true\n' > "$STUBDIR2/pull"
printf '1\n' > "$STUBDIR2/strip_after"; printf '%s\n' "$STRIP_EVENT" > "$STUBDIR2/events"
stub read 200 "$(rb_body)"
run_coord "$DEP"
expect_rc 7 "G3 coord row gone because coord stripped the label"
expect_out "STRIPPED-STALE" "G3"
if [[ "$OUT" == *CONTRADICTION* ]]; then fail "G3: reported as a contradiction :: $OUT"; else ok; fi
no_ok "G3"
# G3b: coord still lists its row, GitHub no longer carries the label: the final
# GitHub read refuses the ok.
gh_fx_reset
printf 'open||false\n' > "$STUBDIR2/pull.1"; printf 'closed||true\n' > "$STUBDIR2/pull"
printf '1\n' > "$STUBDIR2/strip_after"; printf '%s\n' "$STRIP_EVENT" > "$STUBDIR2/events"
stub read 200 "$(rb_body "coord:downstream-of=qontinui/qontinui-web#2281")"
run_coord "$DEP"
expect_rc 7 "G3b stripped after the coord read-back"
no_ok "G3b"
expect_out "coord DOES carry a coord_skill row" "G3b (the row left behind is named)"
# G3c: a missing coord row while the label IS still on GitHub stays a CONTRADICTION.
gh_fx_reset
stub read 200 "$(rb_body)"
run_coord "$DEP"
expect_rc 4 "G3c real contradiction"
expect_out "CONTRADICTION" "G3c"
expect_out "reads present on a fresh read" "G3c"

# --- G4: not a stale strip -> an error, never ok, never STRIPPED-STALE.
# (a) the add changed nothing (no removal recorded); (b) a person removed it and
# the upstream is still open; (c) the PR's events cannot be read.
gh_fx_reset; printf '0\n' > "$STUBDIR2/strip_after"
run_coord "coord:merge-strategy=squash"
expect_rc 3 "G4a absent, no removal recorded"
expect_out "records no removal" "G4a"; no_gh_ok "G4a"; expect_calls post 0 "G4a"
gh_fx_reset; printf '0\n' > "$STUBDIR2/strip_after"
printf '201\t2099-01-01T00:00:01Z\tlabeled\tjspinak\t%s\n202\t2099-01-01T00:00:03Z\tunlabeled\tsomebody\t%s\n' "$DEP" "$DEP" > "$STUBDIR2/events"
run_coord "$DEP"
expect_rc 3 "G4b removed by a person, upstream open"
expect_out "removed by somebody" "G4b"
if [[ "$OUT" == *STRIPPED-STALE* ]]; then fail "G4b: called stale with an open upstream :: $OUT"; else ok; fi
gh_fx_reset; printf '0\n' > "$STUBDIR2/strip_after"; printf '1\n' > "$STUBDIR2/events_rc"
printf 'closed||true\n' > "$STUBDIR2/pull"; printf 'open||false\n' > "$STUBDIR2/pull.1"
run_coord "$DEP"
expect_rc 3 "G4c events unreadable"
expect_out "UNKNOWN" "G4c"
if [[ "$OUT" == *STRIPPED-STALE* ]]; then fail "G4c: stale claimed without a recorded removal :: $OUT"; else ok; fi
# G4c-b: the mark succeeded and only the post-POST events read fails -- the
# message must name THAT, not the mark.
gh_fx_reset; printf '0\n' > "$STUBDIR2/strip_after"; printf '1\n' > "$STUBDIR2/events_rc_post"
printf 'closed||true\n' > "$STUBDIR2/pull"; printf 'open||false\n' > "$STUBDIR2/pull.1"
run_coord "$DEP"
expect_rc 3 "G4c-b post-POST events unreadable"
expect_out "the PR's events could not be read)" "G4c-b"
if [[ "$OUT" == *"before the POST"* ]]; then fail "G4c-b: blamed the pre-POST mark, which succeeded :: $OUT"; else ok; fi
if [[ "$OUT" == *STRIPPED-STALE* ]]; then fail "G4c-b: stale claimed without a readable removal :: $OUT"; else ok; fi

# --- G5: the GitHub read-back itself fails -> exit 8, UNKNOWN, nothing to coord.
gh_fx_reset; printf '1\n' > "$STUBDIR2/labels_rc"
run_coord "coord:stacked-on=#6"
expect_rc 8 "G5 GitHub read-back unreadable"
no_gh_ok "G5"; expect_calls post 0 "G5"; expect_calls probe 0 "G5"

# --- G6: an unreadable upstream does not stop the add; it is named UNKNOWN.
gh_fx_reset; printf '1\n' > "$STUBDIR2/pull_rc"
stub read 200 "$(rb_body "coord:stacked-on=#6")"
run_coord "coord:stacked-on=#6"
expect_rc 0 "G6 upstream unreadable at the pre-flight"
expect_out "has landed is UNKNOWN" "G6"

# --- G7: (G7b and G7f are what pin the event-id mark itself; here a mark of 0
# would read two new adds and exit 3 as well.)
# a removal OLDER than the newest add is not this add's removal, so a
# landed upstream does not make it STRIPPED-STALE: the cause is UNKNOWN, exit 3.
gh_fx_reset
printf 'open||false\n' > "$STUBDIR2/pull.1"; printf 'closed||true\n' > "$STUBDIR2/pull"
printf '0\n' > "$STUBDIR2/strip_after"
printf '101\t2026-11-16T01:00:00Z\tlabeled\tjspinak\t%s\n102\t2026-11-16T01:00:02Z\tunlabeled\tqontinui-merge-orchestrator\t%s\n' "$DEP" "$DEP" > "$STUBDIR2/events_pre"
printf '101\t2026-11-16T01:00:00Z\tlabeled\tjspinak\t%s\n102\t2026-11-16T01:00:02Z\tunlabeled\tqontinui-merge-orchestrator\t%s\n201\t2099-01-01T00:00:01Z\tlabeled\tjspinak\t%s\n' "$DEP" "$DEP" "$DEP" > "$STUBDIR2/events"
run_coord "$DEP"
expect_rc 3 "G7 only an older removal is recorded"
expect_out "no removal of THIS add" "G7"
if [[ "$OUT" == *STRIPPED-STALE* ]]; then fail "G7: an older removal was credited to this add :: $OUT"; else ok; fi

# --- G7b: the new `labeled` event has not appeared yet (events lag), so the
# newest VISIBLE add is an old one, with its old removal after it. That removal
# predates this run's add, so it is not this add's: exit 3, and the landed
# upstream is still NAMED in the message.
gh_fx_reset
printf 'open||false\n' > "$STUBDIR2/pull.1"; printf 'closed||true\n' > "$STUBDIR2/pull"
printf '0\n' > "$STUBDIR2/strip_after"
printf '101\t2026-11-16T01:00:00Z\tlabeled\tjspinak\t%s\n102\t2026-11-16T01:00:02Z\tunlabeled\tqontinui-merge-orchestrator\t%s\n' "$DEP" "$DEP" > "$STUBDIR2/events_pre"; cp "$STUBDIR2/events_pre" "$STUBDIR2/events"
run_coord "$DEP"
expect_rc 3 "G7b lagging labeled event, old removal visible"
if [[ "$OUT" == *STRIPPED-STALE* ]]; then fail "G7b: an old removal was credited to this add :: $OUT"; else ok; fi
expect_out "reads: closed with coord:landed" "G7b (the landed upstream is named)"

# --- G7c: events out of time order are sorted before they are read -- a newer
# removal listed first must still be found.
gh_fx_reset
printf 'open||false\n' > "$STUBDIR2/pull.1"; printf 'closed||true\n' > "$STUBDIR2/pull"
printf '0\n' > "$STUBDIR2/strip_after"
printf '202\t2099-01-01T00:00:03Z\tunlabeled\tqontinui-merge-orchestrator\t%s\n201\t2099-01-01T00:00:01Z\tlabeled\tjspinak\t%s\n' "$DEP" "$DEP" > "$STUBDIR2/events"
run_coord "$DEP"
expect_rc 7 "G7c events listed newest-first"

# --- G7f: an earlier add MINUTES ago (a retry or a peer) with its removal is
# visible before the POST; this run's own labeled event has not appeared yet.
# Its removal is not this add's, whatever the clock says: exit 3.
gh_fx_reset
printf 'open||false\n' > "$STUBDIR2/pull.1"; printf 'closed||true\n' > "$STUBDIR2/pull"
printf '0\n' > "$STUBDIR2/strip_after"
printf '301\t2099-01-01T00:00:00Z\tlabeled\tpeer\t%s\n302\t2099-01-01T00:00:02Z\tunlabeled\tqontinui-merge-orchestrator\t%s\n' "$DEP" "$DEP" > "$STUBDIR2/events_pre"
cp "$STUBDIR2/events_pre" "$STUBDIR2/events"
run_coord "$DEP"
expect_rc 3 "G7f a recent earlier add's removal"
if [[ "$OUT" == *STRIPPED-STALE* ]]; then fail "G7f: an earlier add's removal was credited to this run :: $OUT"; else ok; fi

# --- G7i: ids of different widths sort NUMERICALLY (98 < 99 < 100). A lexical
# sort reads 100 (the removal) before 99 (this run's add), so the removal would
# not follow the add and the credit would be lost.
gh_fx_reset
printf 'open||false\n' > "$STUBDIR2/pull.1"; printf 'closed||true\n' > "$STUBDIR2/pull"
printf '0\n' > "$STUBDIR2/strip_after"
printf '98\t2099-01-01T00:00:00Z\tlabeled\told\t%s\n' "$DEP" > "$STUBDIR2/events_pre"
printf '98\t2099-01-01T00:00:00Z\tlabeled\told\t%s\n99\t2099-01-01T00:00:01Z\tlabeled\tjspinak\t%s\n100\t2099-01-01T00:00:03Z\tunlabeled\tqontinui-merge-orchestrator\t%s\n' "$DEP" "$DEP" "$DEP" > "$STUBDIR2/events"
run_coord "$DEP"
expect_rc 7 "G7i numeric id sort"

# --- G7j: the label is ALREADY on the PR before the POST, so this run's add is
# a no-op that records no event; a new add + removal is someone else's: exit 3.
gh_fx_reset
printf 'open||false\n' > "$STUBDIR2/pull.1"; printf 'closed||true\n' > "$STUBDIR2/pull"
printf '%s\n' "$DEP" > "$STUBDIR2/labels_pre"
printf '201\t2099-01-01T00:00:01Z\tlabeled\tpeer\t%s\n202\t2099-01-01T00:00:03Z\tunlabeled\tqontinui-merge-orchestrator\t%s\n' "$DEP" "$DEP" > "$STUBDIR2/events"
printf '0\n' > "$STUBDIR2/strip_after"
run_coord "$DEP"
expect_rc 3 "G7j label already present before the POST"
expect_out "already on the PR before this run's POST" "G7j (the real reason is named)"
if [[ "$OUT" == *STRIPPED-STALE* ]]; then fail "G7j: a peer's add was credited to a no-op add :: $OUT"; else ok; fi

# --- G7k: the label was ALREADY on the PR and stays there: an ok that says the
# add changed nothing, never "gh added label".
gh_fx_reset
printf '%s\n' "coord:stacked-on=#6" > "$STUBDIR2/labels_pre"
stub read 200 "$(rb_body "coord:stacked-on=#6")"
run_coord "coord:stacked-on=#6"
expect_rc 0 "G7k label already present, still present"
expect_out "was already on $REPO#7" "G7k"
if [[ "$OUT" == *"ok: gh added label"* ]]; then fail "G7k: claimed an add that changed nothing :: $OUT"; else ok; fi

# --- G7k-b: already present, and the first post-POST read lags: the retry's
# ok still says the add changed nothing.
gh_fx_reset
printf '%s\n' "coord:stacked-on=#6" > "$STUBDIR2/labels_pre"
printf 'x\n' > "$STUBDIR2/first_labels_empty"
stub read 200 "$(rb_body "coord:stacked-on=#6")"
run_coord "coord:stacked-on=#6"
expect_rc 0 "G7k-b"
expect_out "it read present before the POST and absent after it" "G7k-b"
if [[ "$OUT" == *"changed nothing"* ]]; then fail "G7k-b: claimed the add changed nothing after reading it absent :: $OUT"; else ok; fi
if [[ "$OUT" == *"ok: gh added label"* ]]; then fail "G7k-b: claimed an add :: $OUT"; else ok; fi

# --- G7l: the pre-POST labels read FAILS: the ok names presence only, never
# "gh added label", and no removal could be tied to this run.
gh_fx_reset
printf 'x\n' > "$STUBDIR2/labels_pre_fail"
stub read 200 "$(rb_body "coord:stacked-on=#6")"
run_coord "coord:stacked-on=#6"
expect_rc 0 "G7l pre-POST labels read failed"
expect_out "whether this run's add changed it is UNKNOWN" "G7l"
if [[ "$OUT" == *"ok: gh added label"* ]]; then fail "G7l: claimed an add nothing read back :: $OUT"; else ok; fi

# --- G7g: TWO new labeled events after the mark (a peer added it too): which
# one is this run's cannot be told, so nothing is credited -- exit 3.
gh_fx_reset
printf 'open||false\n' > "$STUBDIR2/pull.1"; printf 'closed||true\n' > "$STUBDIR2/pull"
printf '0\n' > "$STUBDIR2/strip_after"
printf '201\t2099-01-01T00:00:01Z\tlabeled\tjspinak\t%s\n202\t2099-01-01T00:00:02Z\tlabeled\tpeer\t%s\n203\t2099-01-01T00:00:03Z\tunlabeled\tqontinui-merge-orchestrator\t%s\n' "$DEP" "$DEP" "$DEP" > "$STUBDIR2/events"
run_coord "$DEP"
expect_rc 3 "G7g two new adds"

# --- G7h: the pre-add mark could not be read -> nothing is ours: fail closed.
gh_fx_reset
printf 'open||false\n' > "$STUBDIR2/pull.1"; printf 'closed||true\n' > "$STUBDIR2/pull"
printf '0\n' > "$STUBDIR2/strip_after"; printf '%s\n' "$STRIP_EVENT" > "$STUBDIR2/events"
printf 'x\n' > "$STUBDIR2/events_pre_fail"
run_coord "$DEP"
expect_rc 3 "G7h pre-add mark unreadable"
rm -f "$STUBDIR2/events_pre_fail"

# --- G7d: upstream-of=X makes THIS PR the upstream, so the pre-flight reads it.
gh_fx_reset
printf 'closed||true\n' > "$STUBDIR2/pull"
run_coord "coord:upstream-of=qontinui-runner#1705"
expect_rc 7 "G7d upstream-of on a landed PR"
if grep -q "repos/$REPO/pulls/7 " "$SENTINEL2"; then ok; else fail "G7d: the pre-flight did not read this PR :: $(gh_calls)"; fi

# --- G7e: one lagging read right after the add is retried, not an exit 3.
gh_fx_reset
printf 'x\n' > "$STUBDIR2/first_labels_empty"
stub read 200 "$(rb_body "coord:stacked-on=#6")"
run_coord "coord:stacked-on=#6"
expect_rc 0 "G7e a lagging first read is retried"
expect_out "on a second read" "G7e"
rm -f "$STUBDIR2/first_labels_empty"

# --- G8: GitHub label names are case-insensitive; an existing label in another
# case is what the add attaches, and it reads present.
gh_fx_reset
printf 'Coord:Merge-Strategy=Squash\n' > "$STUBDIR2/labels_list"
stub read 200 "$(rb_body "coord:merge-strategy=squash")"
run_coord "coord:merge-strategy=squash"
expect_rc 0 "G8 differently-cased label reads present"

# --- G9: coord's row is missing AND GitHub cannot be re-read: strip vs lost
# write is UNKNOWN (exit 8), not a CONTRADICTION.
gh_fx_reset
stub read 200 "$(rb_body)"
# the first labels read (step 1) succeeds; every later one fails
run_coord_g9() {
  : > "$STUBDIR3/calls"; : > "$STUBDIR3/bearers"; gh_fx_reset
  printf 'x\n' > "$STUBDIR2/labels_rc_after_1"
  OUT="$(HOME="$FAKEHOME" bash "$SCRIPT" --repo "$REPO" --pr 7 --label "$1" 2>&1)"; RC=$?
}
run_coord_g9 "$DEP"
expect_rc 8 "G9 row missing, GitHub unreadable"
if [[ "$OUT" == *CONTRADICTION* ]]; then fail "G9: reported a contradiction without the GitHub read :: $OUT"; else ok; fi
expect_out "UNKNOWN" "G9"
rm -f "$STUBDIR2/labels_rc_after_1"

# --- G10: the two jq programs the script hands gh run against GitHub-shaped
# JSON. The stub above returns pre-shaped output, so without this a broken
# expression would stay green.
if command -v jq >/dev/null 2>&1; then
  eval "$(grep -E '^(PL_PULL_JQ|GR_EVENTS_JQ)=' "$SCRIPT")"
  got="$(printf '%s' '{"state":"closed","merged_at":null,"labels":[{"name":"x"},{"name":"coord:landed"}]}' | jq -r "$PL_PULL_JQ" | tr -d '\r')"
  [[ "$got" == "closed||true" ]] && ok || fail "G10: PL_PULL_JQ on a rebase-ff land gave [$got]"
  got="$(printf '%s' '{"state":"closed","merged_at":"2026-11-17T09:25:43Z","labels":[]}' | jq -r "$PL_PULL_JQ" | tr -d '\r')"
  [[ "$got" == "closed|2026-11-17T09:25:43Z|false" ]] && ok || fail "G10: PL_PULL_JQ on a merge gave [$got]"
  got="$(printf '%s' '[{"id":11,"event":"labeled","created_at":"T1","actor":{"login":"a"},"label":{"name":"L"}},{"id":12,"event":"commented","created_at":"T2"},{"id":13,"event":"unlabeled","created_at":"T3","actor":null,"label":{"name":"L"}}]' | jq -r "$GR_EVENTS_JQ" | tr -d '\r')"
  [[ "$got" == "$(printf '11\tT1\tlabeled\ta\tL\n13\tT3\tunlabeled\t?\tL')" ]] && ok || fail "G10: GR_EVENTS_JQ gave [$got]"
elif [[ "${SET_LABEL_SELFTEST_NO_JQ:-}" == 1 ]]; then
  echo "note: SET_LABEL_SELFTEST_NO_JQ=1 -- G10 (the jq programs against GitHub-shaped JSON) skipped" >&2
else
  fail "G10: jq is not on PATH, so the jq programs the script hands gh are untested (SET_LABEL_SELFTEST_NO_JQ=1 to skip deliberately)"
fi
gh_fx_reset

# ----- report -----------------------------------------------------------------
if [[ $FAILURES -ne 0 ]]; then
  echo "set-label self-test: $FAILURES failure(s) across $((CHECKS + FAILURES)) assertion(s)" >&2
  exit 1
fi
echo "set-label self-test: $CHECKS assertion(s) classified correctly (ceiling, short-form suggestion, grammar, no-send, dry-run existence caveat, gh success + failure diagnosis, GitHub label read-back + stale-dep strip, owner-tenant proof before the coord write, read-back after it)"
