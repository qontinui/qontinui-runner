#!/usr/bin/env bash
# reap-restore-snapshot-test.sh — suite for reap-restore-snapshot.sh, the ONE
# deletion path for a checkout-restore snapshot (refs/wip/return-to-main/*) and
# the parked branch it recovers.
#
# Plan: 2026-09-13-one-recovery-rule-for-both-checkout-restorers, Phase 1.
#
# The subject's header is the contract; every case below asserts the exit code,
# the `outcome`, and WHICH REFS EXIST AFTERWARDS (branch, snapshot and, where it
# matters, the `branch.<b>.*` config section). A reaper that answers correctly
# and deletes the wrong thing is the failure this suite exists for.
#
#   c01  plain landed (tip an ancestor of origin/main)       -> REAPED, all gone
#   c02  reflog-only commit via `branch -f` there and back   -> SNAPSHOT_ONLY (check 6)
#   c03  amended-away commit on a checked-out branch         -> SNAPSHOT_ONLY
#   c04  rebased branch, pre-rebase commit patch-equivalent  -> REAPED
#   c05  content landed as a cherry-pick, not an ancestor    -> REAPED via 5b
#   c06  branch moved but still contains the sha (and that
#        move landed)                                        -> SNAPSHOT_ONLY
#        branch reset to an unrelated commit, sole holder    -> REFUSED
#   c07  branch deleted, sha unlanded and held by no branch  -> REFUSED
#   c08  branch checked out again (main worktree / linked)   -> SNAPSHOT_ONLY
#   c09  ref moved between check and delete (REAP_RACE_HOOK)  -> REFUSED
#   c10  wrong device                                        -> DEFERRED (5)
#   c11  fetch fails                                         -> UNKNOWN (3)
#   c12  squash land through the `pr #<n>` arm (land-evidence stubbed):
#        PROVEN -> REAPED; NONE -> not deletable; exit 3 -> UNKNOWN;
#        no `pr #` -> the stub is never called
#   c13  manual message without `leaving <b>`: unique tip match -> REAPED;
#        ambiguous -> SNAPSHOT_ONLY; none + unlanded -> REFUSED
#   c14  residue: upstream-historical, EOL-only, bundle path (with and without
#        a runner repo), novel blob, staged change, no origin/<default>
#   c15  ABSENT, malformed name, target/name sha mismatch, default-branch
#        fast-forward snapshot
#   c16  usage
#   c09b REAP_RACE_HOOK is ignored without REAP_TEST_SEAMS=1
#   c14h residue whose R^1 no ref holds -> REFUSED; c14i a local branch holds it -> REAPED
#   c17  check 6 covers ANCESTORS of reflog-named commits    -> SNAPSHOT_ONLY
#   c18  verbatim patch equivalence: whitespace-only divergence at the tip
#        (check 5) and as a reflog-only commit (check 6)     -> SNAPSHOT_ONLY
#   c19  check 3 sees a linked worktree mid `rebase -i` and a main dir
#        mid-bisect on the branch                            -> SNAPSHOT_ONLY
#   c20  empty snapshot message + unique landed tip match    -> SNAPSHOT_ONLY
#   c21  check 3 re-probed before the delete: a checkout / a bisect started
#        through REAP_RACE_HOOK                              -> REFUSED
#   c22  a lossy textconv driver: hides a reflog-only difference (check 6) ->
#        SNAPSHOT_ONLY; the same at the tip (5b, decided by the cherry filter)
#   c23  check 5b's range rev-list fails (git shim)          -> UNKNOWN; control REAPED
#   c24  patch-id fails (git shim) in check 6 and in 5b     -> UNKNOWN; controls REAPED
#   c25  a linked worktree's `rebase -i --update-refs` lists the branch -> SNAPSHOT_ONLY
#
# HERMETIC: every repository is under a mktemp sandbox, each case has its own
# local bare `origin` so the subject's `git fetch` works offline, HOME points
# into the sandbox (no ~/.qontinui/machine.json), and land-evidence.sh is always
# a stub. The mutation section re-runs this file against mutated copies of the
# subject through scripts/lib/mutation-control.sh, with an UNMUTATED control
# copy staged the same way (the copy sits alone in a temp dir, so REAP_LIB_DIR
# and REAP_LAND_EVIDENCE are passed explicitly).
#
# REAP_TEST_ONLY="c04 c09" runs only those cases (and skips mutation control).
# REAP_MUTATION_ONLY=1 runs ONLY the mutation control (control copy + mutants),
# so a slow box can run the suite in two chunks; every re-run it drives clears it.
# REAP_MUTANT_ONLY="M0 Ma Mf" narrows that section to those ids (M0 = the control).
# Exit: 0 every assertion held and every declared mutation reddened; 1 otherwise.

set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
SELF="$HERE/$(basename "${BASH_SOURCE[0]}")"
SUITE_REL=".claude/skills/return-to-main/reap-restore-snapshot-test.sh"
REAL_SUBJECT="$HERE/reap-restore-snapshot.sh"
SUBJECT="${MC_SUBJECT:-$REAL_SUBJECT}"

[ -f "$SUBJECT" ] || { echo "reap-restore-snapshot-test: subject not found: $SUBJECT" >&2; exit 1; }
[ -d "$ROOT/scripts/lib" ] || { echo "reap-restore-snapshot-test: $ROOT/scripts/lib not found" >&2; exit 1; }
command -v git >/dev/null 2>&1 || { echo "reap-restore-snapshot-test: git not on PATH" >&2; exit 1; }

# shellcheck source=../../../scripts/lib/mutation-control.sh
. "$ROOT/scripts/lib/mutation-control.sh"

PASS=0; FAIL=0
ok()  { PASS=$((PASS + 1)); printf '  ok   %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf '  FAIL %s\n' "$1"; [ $# -gt 1 ] && printf '       %s\n' "$2"; return 0; }
eq()  { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1" "expected [$2] got [$3]"; fi; }
has() { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "[$2] not in [${3:0:600}]" ;; esac; }
# The negative twin of `has`. Added with c12f: a `lacks` call in a suite that
# had none is a "command not found" on stderr and a silently VACUOUS assertion,
# which is worse than no assertion at all.
lacks() { case "$3" in *"$2"*) bad "$1" "[$2] present in [${3:0:600}]" ;; *) ok "$1" ;; esac; }

want() {
  [ "${REAP_MUTATION_ONLY:-}" = 1 ] && return 1
  [ -z "${REAP_TEST_ONLY:-}" ] && return 0
  case " $REAP_TEST_ONLY " in *" $1 "*) return 0 ;; esac
  return 1
}

SANDBOX="$(mktemp -d)" || { echo "reap-restore-snapshot-test: mktemp failed" >&2; exit 1; }
trap 'rm -rf "$SANDBOX"' EXIT
mc_init "$SUITE_REL" "$SANDBOX"
nw() { if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s' "$1"; fi; }

export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_COUNT=6
export GIT_CONFIG_KEY_0=user.name GIT_CONFIG_VALUE_0=reap-test
export GIT_CONFIG_KEY_1=user.email GIT_CONFIG_VALUE_1=reap-test@example.invalid
export GIT_CONFIG_KEY_2=core.autocrlf GIT_CONFIG_VALUE_2=false
export GIT_CONFIG_KEY_3=commit.gpgsign GIT_CONFIG_VALUE_3=false
export GIT_CONFIG_KEY_4=init.defaultBranch GIT_CONFIG_VALUE_4=main
mkdir -p "$SANDBOX/nohooks" "$SANDBOX/home" "$SANDBOX/bin"
export GIT_CONFIG_KEY_5=core.hooksPath GIT_CONFIG_VALUE_5="$(nw "$SANDBOX/nohooks")"
export HOME="$SANDBOX/home"
unset QONTINUI_MACHINE_ID QONTINUI_RUNNER_REPO REAP_RACE_HOOK REAP_TEST_SEAMS GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR
export REAP_LIB_DIR="$ROOT/scripts/lib"

# --- land-evidence stubs ------------------------------------------------------
LE_MARK="$SANDBOX/le-called"
cat >"$SANDBOX/bin/le-loud.sh" <<EOF
#!/bin/bash
echo "CALLED \$*" >>"$LE_MARK"
echo "land-evidence stub: must not be called in this case" >&2
exit 99
EOF
cat >"$SANDBOX/bin/le-proven.sh" <<EOF
#!/bin/bash
echo "CALLED \$*" >>"$LE_MARK"
printf '{"evidence_strength":"PROVEN_LANDED","commits":[{"sha":"%s","candidates":[{"kind":"github_merged_pr","pr":7,"full_presence":true}]}]}\n' "\$LE_TIP"
exit 0
EOF
cat >"$SANDBOX/bin/le-none.sh" <<EOF
#!/bin/bash
echo "CALLED \$*" >>"$LE_MARK"
printf '{"evidence_strength":"NONE","commits":[{"sha":"%s","candidates":[]}]}\n' "\$LE_TIP"
exit 2
EOF
cat >"$SANDBOX/bin/le-unknown.sh" <<EOF
#!/bin/bash
echo "CALLED \$*" >>"$LE_MARK"
echo "land-evidence stub: github unreachable" >&2
printf '{"evidence_strength":"UNKNOWN"}\n'
exit 3
EOF
cat >"$SANDBOX/bin/le-superseded.sh" <<EOF
#!/bin/bash
echo "CALLED \$*" >>"$LE_MARK"
printf '{"evidence_strength":"SUPERSEDED","land_signal":false,"commits":[{"sha":"%s","candidates":[],"verdict":"SUPERSEDED"}]}\n' "\$LE_TIP"
exit 5
EOF
export REAP_LAND_EVIDENCE="$SANDBOX/bin/le-loud.sh"

# --- fixture helpers ------------------------------------------------------------
STAMP=20260901T020304Z
g()  { local d="$1"; shift; git -C "$(nw "$d")" "$@"; }
gq() { g "$@" >/dev/null 2>&1; }
put() { mkdir -p "$(dirname "$1/$2")"; printf '%b' "$3" >"$1/$2"; gq "$1" add -- "$2"; }
cm()  { gq "$1" commit -q -m "$2"; g "$1" rev-parse HEAD; }

# newcase <id>: $C = case dir, $R = checkout (basename == <id>), $BASE pushed as origin/main.
newcase() {
  C="$SANDBOX/$1"; R="$C/$1"; mkdir -p "$R"
  git init -q --bare "$(nw "$C/origin.git")"
  git init -q "$(nw "$R")"
  gq "$R" remote add origin "$(nw "$C/origin.git")"
  put "$R" base.txt 'base\n'; BASE="$(cm "$R" base)"
  gq "$R" push -q origin main
}
# branchcommit <dir> <branch> <file> <content> <msg>: a commit on a new branch from
# the current HEAD, then back on main. Prints the tip.
branchcommit() {
  gq "$1" checkout -q -b "$2"; put "$1" "$3" "$4"; cm "$1" "$5"; gq "$1" checkout -q main
}
push_to_main() { gq "$1" push -q -f origin "$2:refs/heads/main"; }

sweep_msg() { printf 'return-to-main-sweep: %s leaving %s (verdict LANDED_DUPLICATE, upstream origin/main last fetched 2026-09-01T02:00:00Z, tip committed 2026-08-30T10:00:00Z)' "$1" "$2"; }
rd_msg()    { printf 'restore-default: %s leaving %s (verdict restore_default, upstream origin/main last fetched 2026-09-01T02:00:00Z, tip committed 2026-08-30T10:00:00Z) pr #%s' "$1" "$2" "$3"; }
rd_msg_nopr() { printf 'restore-default: %s leaving %s (verdict restore_default, upstream origin/main last fetched 2026-09-01T02:00:00Z, tip committed 2026-08-30T10:00:00Z)' "$1" "$2"; }

# snap <dir> <sha> <message> [stamp]: writes the snapshot exactly as the writers do; prints the ref.
snap() {
  local ref="refs/wip/return-to-main/${4:-$STAMP}-$(basename "$1")-${2:0:7}"
  gq "$1" update-ref --create-reflog -m "$3" "$ref" "$2" || echo "FIXTURE: update-ref $ref failed" >&2
  printf '%s' "$ref"
}
# residue <dir> <stamp> <file> <content> [stage]: a `git stash create` residue snapshot; tree left clean.
residue() {
  local d="$1" ref="refs/wip/return-to-main/$2-$(basename "$1")-residue" s
  printf '%b' "$4" >"$d/$3"
  [ -n "${5:-}" ] && gq "$d" add -- "$3"
  s="$(g "$d" stash create 2>/dev/null)"
  [ -n "$s" ] || echo "FIXTURE: stash create produced nothing for $3" >&2
  gq "$d" update-ref --create-reflog -m "return-to-main-sweep: $(basename "$d") residue restore of $3" "$ref" "$s"
  gq "$d" reset -q -- "$3"
  gq "$d" checkout -- "$3"
  printf '%s' "$ref"
}

# reap <dir> <ref> [extra args]: runs the subject; sets OUT, RC, NLINES.
reap() {
  local mid="${MID-dev-1}"
  OUT="$(QONTINUI_MACHINE_ID="$mid" bash "$SUBJECT" "$1" "$2" --device "${DEV:-dev-1}" --fetch-timeout 60 "${@:3}" 2>"$SANDBOX/last.err")"; RC=$?
  NLINES="$(printf '%s' "$OUT" | grep -c .)"
}
jf()  { printf '%s' "$OUT" | grep -o "\"$1\":\"[^\"]*\"" | head -1 | sed 's/^"[^"]*":"//; s/"$//'; }
chk() { printf '%s' "$OUT" | grep -o "{\"check\":\"$1\",\"result\":\"[a-z]*\"" | head -1 | sed 's/.*"result":"//; s/"$//'; }
exists() { if gq "$1" show-ref --verify --quiet "$2"; then echo yes; else echo no; fi; }
outcome_is() { # <label> <rc> <outcome>
  eq "$1: exit" "$2" "$RC"
  eq "$1: outcome" "$3" "$(jf outcome)"
}

# ================================================================================
if want c01; then
  echo "c01 plain landed"
  newcase c01
  gq "$R" checkout -q -b feat; put "$R" f.txt 'f1\n'; cm "$R" f1 >/dev/null; put "$R" f.txt 'f1\nf2\n'; TIP="$(cm "$R" f2)"
  gq "$R" config branch.feat.remote origin; gq "$R" config branch.feat.merge refs/heads/feat
  gq "$R" checkout -q main; push_to_main "$R" "$TIP"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c01 feat)")"
  reap "$R" "$REF"
  outcome_is "c01" 0 REAPED
  eq "c01: exactly one stdout line" 1 "$NLINES"
  eq "c01: branch feat deleted" no "$(exists "$R" refs/heads/feat)"
  eq "c01: snapshot deleted" no "$(exists "$R" "$REF")"
  eq "c01: branch.feat.* config removed" "" "$(g "$R" config --local --get-regexp '^branch\.feat\.' 2>/dev/null)"
  eq "c01: branch resolved from the message" message "$(jf branch_source)"
  eq "c01: work_outcome" work_completed "$(jf work_outcome)"
fi

if want c02; then
  echo "c02 reflog-only commit via branch -f there and back"
  newcase c02
  TIP="$(branchcommit "$R" feat f.txt 'f1\n' f1)"
  gq "$R" checkout -q --detach "$TIP"; put "$R" stray.txt 'only in the reflog\n'; STRAY="$(cm "$R" stray)"; gq "$R" checkout -q main
  gq "$R" branch -f feat "$STRAY"; gq "$R" branch -f feat "$TIP"
  push_to_main "$R" "$TIP"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c02 feat)")"
  reap "$R" "$REF"
  outcome_is "c02" 1 SNAPSHOT_ONLY
  eq "c02: check 6 failed" fail "$(chk 6_reflog_only_empty)"
  eq "c02: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
  eq "c02: snapshot deleted" no "$(exists "$R" "$REF")"
  eq "c02: the stray commit is still reachable from feat's reflog" "$STRAY" "$(g "$R" log -g --format=%H refs/heads/feat -- | grep -x "$STRAY")"
fi

if want c03; then
  echo "c03 amended-away commit on a checked-out branch"
  newcase c03
  gq "$R" checkout -q -b feat; put "$R" f.txt 'first draft\n'; cm "$R" draft >/dev/null
  put "$R" f.txt 'final version\n'; gq "$R" commit -q --amend -m final; TIP="$(g "$R" rev-parse HEAD)"
  gq "$R" checkout -q main; push_to_main "$R" "$TIP"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c03 feat)")"
  reap "$R" "$REF"
  outcome_is "c03" 1 SNAPSHOT_ONLY
  eq "c03: check 6 failed" fail "$(chk 6_reflog_only_empty)"
  eq "c03: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
  eq "c03: snapshot deleted" no "$(exists "$R" "$REF")"
fi

if want c04; then
  echo "c04 rebased branch whose pre-rebase commit is patch-equivalent"
  newcase c04
  F1="$(branchcommit "$R" feat f.txt 'feature\n' f1)"
  put "$R" m.txt 'mainline\n'; cm "$R" m1 >/dev/null; gq "$R" push -q origin main
  gq "$R" checkout -q feat; gq "$R" rebase -q main; TIP="$(g "$R" rev-parse HEAD)"; gq "$R" checkout -q main
  [ "$TIP" != "$F1" ] || bad "c04: FIXTURE the rebase did not rewrite F1"
  push_to_main "$R" "$TIP"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c04 feat)")"
  reap "$R" "$REF"
  outcome_is "c04" 0 REAPED
  eq "c04: check 6 passed" pass "$(chk 6_reflog_only_empty)"
  has "c04: the pre-rebase commit is the one reflog-only commit, and it is verbatim-equivalent" "1 reflog-only commit(s), all 1 verbatim-equivalent" "$OUT"
  eq "c04: branch feat deleted" no "$(exists "$R" refs/heads/feat)"
  eq "c04: snapshot deleted" no "$(exists "$R" "$REF")"
fi

if want c05; then
  echo "c05 content landed as a cherry-pick, not an ancestor"
  newcase c05
  TIP="$(branchcommit "$R" feat f.txt 'feature\n' "feat: f")"
  put "$R" m.txt 'other work\n'; cm "$R" m1 >/dev/null
  gq "$R" cherry-pick "$TIP"; gq "$R" push -q origin main
  REF="$(snap "$R" "$TIP" "$(sweep_msg c05 feat)")"
  reap "$R" "$REF"
  outcome_is "c05" 0 REAPED
  has "c05: landed through arm 5b" '"detail":"5b:' "$OUT"
  eq "c05: branch feat deleted" no "$(exists "$R" refs/heads/feat)"
  eq "c05: snapshot deleted" no "$(exists "$R" "$REF")"
fi

if want c06; then
  echo "c06a branch moved since the snapshot, still contains it (and the move landed)"
  newcase c06a
  TIP="$(branchcommit "$R" feat f.txt 'f1\n' f1)"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c06a feat)")"
  gq "$R" checkout -q feat; put "$R" f.txt 'f1\nlater\n'; MOVED="$(cm "$R" later)"; gq "$R" checkout -q main
  push_to_main "$R" "$MOVED"
  reap "$R" "$REF"
  outcome_is "c06a" 1 SNAPSHOT_ONLY
  eq "c06a: check 4 failed" fail "$(chk 4_tip_unchanged)"
  eq "c06a: branch feat kept at the moved tip" "$MOVED" "$(g "$R" rev-parse --verify --quiet refs/heads/feat)"
  eq "c06a: snapshot deleted" no "$(exists "$R" "$REF")"

  echo "c06b branch reset to an unrelated commit, snapshot is the sole holder"
  newcase c06b
  TIP="$(branchcommit "$R" feat f.txt 'unlanded\n' f1)"
  put "$R" m.txt 'mainline\n'; M1="$(cm "$R" m1)"; gq "$R" push -q origin main
  REF="$(snap "$R" "$TIP" "$(sweep_msg c06b feat)")"
  gq "$R" branch -f feat "$M1"
  reap "$R" "$REF"
  outcome_is "c06b" 2 REFUSED
  has "c06b: reason names the sole holder" "the snapshot is the only ref holding" "$(jf reason)"
  eq "c06b: branch feat kept at the reset commit" "$M1" "$(g "$R" rev-parse --verify --quiet refs/heads/feat)"
  eq "c06b: snapshot kept" yes "$(exists "$R" "$REF")"
  eq "c06b: deleted[] empty" '"deleted":[]' "$(printf '%s' "$OUT" | grep -o '"deleted":\[[^]]*\]')"
fi

if want c07; then
  echo "c07 branch deleted by someone, sha unlanded"
  newcase c07
  TIP="$(branchcommit "$R" feat f.txt 'unlanded\n' f1)"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c07 feat)")"
  gq "$R" update-ref -d refs/heads/feat
  reap "$R" "$REF"
  outcome_is "c07" 2 REFUSED
  has "c07: reason names the missing branch" "refs/heads/feat no longer exists" "$(jf reason)"
  eq "c07: snapshot kept" yes "$(exists "$R" "$REF")"
  eq "c07: work_outcome is abandoned/refused" "work_abandoned: refused:" "$(jf work_outcome | cut -c1-24)"
fi

if want c08; then
  echo "c08a branch checked out again in the main worktree"
  newcase c08a
  TIP="$(branchcommit "$R" feat f.txt 'f1\n' f1)"; push_to_main "$R" "$TIP"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c08a feat)")"
  gq "$R" checkout -q feat
  reap "$R" "$REF"
  outcome_is "c08a" 1 SNAPSHOT_ONLY
  eq "c08a: check 3 failed" fail "$(chk 3_not_checked_out)"
  eq "c08a: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
  eq "c08a: snapshot deleted" no "$(exists "$R" "$REF")"

  echo "c08b branch checked out in a LINKED worktree"
  newcase c08b
  TIP="$(branchcommit "$R" feat f.txt 'f1\n' f1)"; push_to_main "$R" "$TIP"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c08b feat)")"
  gq "$R" worktree add -q "$(nw "$C/linked")" feat || bad "c08b: FIXTURE worktree add failed"
  reap "$R" "$REF"
  outcome_is "c08b" 1 SNAPSHOT_ONLY
  eq "c08b: check 3 failed" fail "$(chk 3_not_checked_out)"
  eq "c08b: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
fi

if want c09; then
  echo "c09 ref moved between check and delete"
  newcase c09
  TIP="$(branchcommit "$R" feat f.txt 'f1\n' f1)"; push_to_main "$R" "$TIP"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c09 feat)")"
  REAP_TEST_SEAMS=1 REAP_RACE_HOOK="git -C '$(nw "$R")' update-ref refs/heads/feat $BASE" reap "$R" "$REF"
  outcome_is "c09" 2 REFUSED
  has "c09: reason names the race" "moved between check and delete" "$(jf reason)"
  eq "c09: check 6 had passed (the race is what refused)" pass "$(chk 6_reflog_only_empty)"
  eq "c09: branch feat still exists at the moved value" "$BASE" "$(g "$R" rev-parse --verify --quiet refs/heads/feat)"
  eq "c09: snapshot kept" yes "$(exists "$R" "$REF")"

  echo "c09b REAP_RACE_HOOK without REAP_TEST_SEAMS=1 is ignored"
  newcase c09b
  TIP="$(branchcommit "$R" feat f.txt 'f1\n' f1)"; push_to_main "$R" "$TIP"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c09b feat)")"
  HOOKMARK="$C/hook-ran"
  REAP_TEST_SEAMS='' REAP_RACE_HOOK="touch '$HOOKMARK'; git -C '$(nw "$R")' update-ref refs/heads/feat $BASE" reap "$R" "$REF"
  outcome_is "c09b" 0 REAPED
  eq "c09b: the hook did not run" no "$([ -e "$HOOKMARK" ] && echo yes || echo no)"
  eq "c09b: branch feat deleted" no "$(exists "$R" refs/heads/feat)"
  eq "c09b: snapshot deleted" no "$(exists "$R" "$REF")"
fi

if want c10; then
  echo "c10 wrong device"
  newcase c10
  TIP="$(branchcommit "$R" feat f.txt 'f1\n' f1)"; push_to_main "$R" "$TIP"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c10 feat)")"
  MID=this-device DEV=other-device reap "$R" "$REF"
  outcome_is "c10" 5 DEFERRED
  eq "c10: work_outcome" "work_abandoned: wrong_device" "$(jf work_outcome)"
  eq "c10: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
  eq "c10: snapshot kept" yes "$(exists "$R" "$REF")"
  eq "c10: no fetch was attempted" 'false' "$(printf '%s' "$OUT" | grep -o '"fetched":[a-z]*' | cut -d: -f2)"
fi

if want c11; then
  echo "c11 fetch failure"
  newcase c11
  TIP="$(branchcommit "$R" feat f.txt 'f1\n' f1)"; push_to_main "$R" "$TIP"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c11 feat)")"
  gq "$R" remote set-url origin "$(nw "$C/does-not-exist.git")"
  reap "$R" "$REF"
  outcome_is "c11" 3 UNKNOWN
  has "c11: reason names the fetch" "fetch failed" "$(jf reason)"
  eq "c11: fetch check unknown" unknown "$(chk fetch)"
  eq "c11: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
  eq "c11: snapshot kept" yes "$(exists "$R" "$REF")"
fi

if want c12; then
  # A squash land: the branch has two commits, origin/main one squash commit
  # patch-equivalent to neither.
  mksquash() { # <id> -> R, TIP
    newcase "$1"
    gq "$R" checkout -q -b feat
    put "$R" a.txt 'one\n'; cm "$R" s1 >/dev/null
    put "$R" a.txt 'one\ntwo\n'; put "$R" b.txt 'bee\n'; TIP="$(cm "$R" s2)"
    gq "$R" checkout -q main
    gq "$R" merge -q --squash feat; cm "$R" "feat: squash (#7)" >/dev/null
    gq "$R" push -q origin main
  }
  echo "c12a squash-merged restore-default, land-evidence PROVEN through PR #7"
  mksquash c12a; rm -f "$LE_MARK"
  REF="$(snap "$R" "$TIP" "$(rd_msg c12a feat 7)")"
  LE_TIP="$TIP" REAP_LAND_EVIDENCE="$SANDBOX/bin/le-proven.sh" reap "$R" "$REF"
  outcome_is "c12a" 0 REAPED
  eq "c12a: pr parsed" 7 "$(jf pr)"
  eq "c12a: writer" restore-default "$(jf writer)"
  has "c12a: landed through arm 5c" '"detail":"5c:' "$OUT"
  has "c12a: the stub was asked about the tip" "--ref $TIP" "$(cat "$LE_MARK" 2>/dev/null)"
  eq "c12a: branch feat deleted" no "$(exists "$R" refs/heads/feat)"
  eq "c12a: snapshot deleted" no "$(exists "$R" "$REF")"

  # Header OUTCOMES: SNAPSHOT_ONLY when "the branch is not deletable ... but a
  # local branch ... still contains the snapshot sha". feat itself still holds
  # the tip here, so a NONE verdict keeps the branch and drops the snapshot.
  echo "c12b land-evidence NONE"
  mksquash c12b; rm -f "$LE_MARK"
  REF="$(snap "$R" "$TIP" "$(rd_msg c12b feat 7)")"
  LE_TIP="$TIP" REAP_LAND_EVIDENCE="$SANDBOX/bin/le-none.sh" reap "$R" "$REF"
  outcome_is "c12b" 1 SNAPSHOT_ONLY
  eq "c12b: check 5 failed" fail "$(chk 5_landed)"
  eq "c12b: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
  eq "c12b: the stub was called" yes "$([ -s "$LE_MARK" ] && echo yes || echo no)"

  echo "c12c land-evidence exits 3"
  mksquash c12c; rm -f "$LE_MARK"
  REF="$(snap "$R" "$TIP" "$(rd_msg c12c feat 7)")"
  LE_TIP="$TIP" REAP_LAND_EVIDENCE="$SANDBOX/bin/le-unknown.sh" reap "$R" "$REF"
  outcome_is "c12c" 3 UNKNOWN
  has "c12c: reason names land-evidence exit 3" "land-evidence.sh for PR #7 exited 3" "$(jf reason)"
  eq "c12c: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
  eq "c12c: snapshot kept" yes "$(exists "$R" "$REF")"

  # c12f: exit 5 SUPERSEDED. This reaper DELETES a snapshot and its parked
  # branch, so 5c mistaking supersession for a land would destroy the only
  # surviving copy of that content. The exit already fell into the `-ge 4`
  # catch-all and so already read UNKNOWN -- safe, but BY ACCIDENT and pinned by
  # nothing, which is the unpinned-control shape plan
  # 2026-09-16-land-evidence-has-no-superseded-arm exists to object to. Pin it.
  echo "c12f land-evidence exits 5 SUPERSEDED: never a land, and the reason says why"
  mksquash c12f; rm -f "$LE_MARK"
  REF="$(snap "$R" "$TIP" "$(rd_msg c12f feat 7)")"
  LE_TIP="$TIP" REAP_LAND_EVIDENCE="$SANDBOX/bin/le-superseded.sh" reap "$R" "$REF"
  outcome_is "c12f" 3 UNKNOWN
  lacks "c12f: NOT recorded as landed through arm 5c" '"detail":"5c:' "$OUT"
  has "c12f: the reason names SUPERSEDED rather than a bare exit code" "SUPERSEDED (exit 5)" "$(jf reason)"
  has "c12f: ... and says upstream moving past a branch is not a land" "not evidence its content reached" "$(jf reason)"
  eq "c12f: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
  eq "c12f: snapshot kept -- a superseded branch is never reaped on 5c" yes "$(exists "$R" "$REF")"

  echo "c12d no pr # and no land: the stub is never called"
  mksquash c12d; rm -f "$LE_MARK"
  REF="$(snap "$R" "$TIP" "$(rd_msg_nopr c12d feat)")"
  reap "$R" "$REF"
  outcome_is "c12d" 1 SNAPSHOT_ONLY
  eq "c12d: check 5 failed" fail "$(chk 5_landed)"
  eq "c12d: land-evidence was not called" no "$([ -e "$LE_MARK" ] && echo yes || echo no)"
  eq "c12d: branch feat kept" yes "$(exists "$R" refs/heads/feat)"

  echo "c12e no pr #, no land, branch gone: sole holder"
  mksquash c12e; rm -f "$LE_MARK"
  REF="$(snap "$R" "$TIP" "$(rd_msg_nopr c12e feat)")"
  gq "$R" update-ref -d refs/heads/feat
  reap "$R" "$REF"
  outcome_is "c12e" 2 REFUSED
  has "c12e: reason names the sole holder" "the snapshot is the only ref holding" "$(jf reason)"
  eq "c12e: land-evidence was not called" no "$([ -e "$LE_MARK" ] && echo yes || echo no)"
  eq "c12e: snapshot kept" yes "$(exists "$R" "$REF")"
fi

if want c13; then
  MANUAL="manual return-to-main: moved by hand"
  echo "c13a manual message, unique tip match, landed"
  newcase c13a
  TIP="$(branchcommit "$R" feat f.txt 'f1\n' f1)"; push_to_main "$R" "$TIP"
  REF="$(snap "$R" "$TIP" "$MANUAL")"
  reap "$R" "$REF"
  outcome_is "c13a" 0 REAPED
  eq "c13a: branch_source" tip_match "$(jf branch_source)"
  eq "c13a: branch" feat "$(jf branch)"
  eq "c13a: branch feat deleted" no "$(exists "$R" refs/heads/feat)"
  eq "c13a: snapshot deleted" no "$(exists "$R" "$REF")"

  echo "c13b manual message, two branches at the tip"
  newcase c13b
  TIP="$(branchcommit "$R" feat f.txt 'f1\n' f1)"; gq "$R" branch feat2 "$TIP"; push_to_main "$R" "$TIP"
  REF="$(snap "$R" "$TIP" "$MANUAL")"
  reap "$R" "$REF"
  outcome_is "c13b" 1 SNAPSHOT_ONLY
  eq "c13b: branch_source" unresolved_ambiguous_tip_match "$(jf branch_source)"
  eq "c13b: feat kept" yes "$(exists "$R" refs/heads/feat)"
  eq "c13b: feat2 kept" yes "$(exists "$R" refs/heads/feat2)"
  eq "c13b: snapshot deleted" no "$(exists "$R" "$REF")"

  echo "c13c manual message, no branch at the tip, unlanded"
  newcase c13c
  gq "$R" checkout -q --detach; put "$R" f.txt 'detached\n'; TIP="$(cm "$R" detached)"; gq "$R" checkout -q main
  REF="$(snap "$R" "$TIP" "$MANUAL")"
  reap "$R" "$REF"
  outcome_is "c13c" 2 REFUSED
  has "c13c: reason names the sole holder" "the snapshot is the only ref holding" "$(jf reason)"
  eq "c13c: branch_source" unresolved_no_tip_match "$(jf branch_source)"
  eq "c13c: snapshot kept" yes "$(exists "$R" "$REF")"
fi

if want c14; then
  newcase c14
  put "$R" f.txt 'one\n'; cm "$R" v1 >/dev/null
  put "$R" f.txt 'second\n'; cm "$R" v2 >/dev/null
  put "$R" .claude/skills/x/SKILL.md 'local skill\n'; cm "$R" skill >/dev/null
  gq "$R" push -q origin main
  # The fake runner repo whose origin/main history holds the bundle blob.
  RUN="$C/runner"; mkdir -p "$RUN"
  git init -q --bare "$(nw "$C/runner-origin.git")"; git init -q "$(nw "$RUN")"
  gq "$RUN" remote add origin "$(nw "$C/runner-origin.git")"
  put "$RUN" src-tauri/src/fleet_skills/x/SKILL.md 'bundled skill content\n'; cm "$RUN" bundle >/dev/null
  gq "$RUN" push -q origin main
  NORUN="$C/no-runner-here"

  echo "c14a residue blob is upstream-historical"
  REF="$(residue "$R" 20260901T000001Z f.txt 'one\n')"
  QONTINUI_RUNNER_REPO="$NORUN" reap "$R" "$REF"
  outcome_is "c14a" 0 REAPED
  eq "c14a: shape" residue "$(jf shape)"
  eq "c14a: residue ref deleted" no "$(exists "$R" "$REF")"

  echo "c14b EOL-only residue"
  REF="$(residue "$R" 20260901T000002Z f.txt 'second\r\n')"
  QONTINUI_RUNNER_REPO="$NORUN" reap "$R" "$REF"
  outcome_is "c14b" 0 REAPED
  has "c14b: passed through the EOL_ONLY arm" "EOL_ONLY" "$OUT"
  eq "c14b: residue ref deleted" no "$(exists "$R" "$REF")"

  echo "c14c novel blob"
  REF="$(residue "$R" 20260901T000003Z f.txt 'novel content nobody has\n')"
  QONTINUI_RUNNER_REPO="$NORUN" reap "$R" "$REF"
  outcome_is "c14c" 2 REFUSED
  has "c14c: reason names unprovable residue" "not provable residue: f.txt" "$(jf reason)"
  eq "c14c: residue ref kept" yes "$(exists "$R" "$REF")"

  echo "c14d bundle-path blob held by the runner's origin/main history"
  REF="$(residue "$R" 20260901T000004Z .claude/skills/x/SKILL.md 'bundled skill content\n')"
  QONTINUI_RUNNER_REPO="$RUN" reap "$R" "$REF"
  outcome_is "c14d" 0 REAPED
  has "c14d: passed through the RUNNER_BUNDLE arm" "RUNNER_BUNDLE" "$OUT"
  eq "c14d: residue ref deleted" no "$(exists "$R" "$REF")"

  echo "c14e the same bundle-path blob with no runner repo"
  REF="$(residue "$R" 20260901T000005Z .claude/skills/x/SKILL.md 'bundled skill content\n')"
  QONTINUI_RUNNER_REPO="$NORUN" reap "$R" "$REF"
  outcome_is "c14e" 3 UNKNOWN
  has "c14e: reason names the undecided path" "could not be decided" "$(jf reason)"
  has "c14e: the missing runner repo is named" "no qontinui-runner checkout" "$OUT"
  eq "c14e: residue ref kept" yes "$(exists "$R" "$REF")"

  echo "c14g staged change (R^2^{tree} differs from R^1^{tree}), content upstream-historical"
  REF="$(residue "$R" 20260901T000006Z f.txt 'one\n' stage)"
  QONTINUI_RUNNER_REPO="$NORUN" reap "$R" "$REF"
  outcome_is "c14g" 2 REFUSED
  has "c14g: reason names staged changes" "carries staged changes" "$(jf reason)"
  eq "c14g: residue_shape failed" fail "$(chk residue_shape)"
  eq "c14g: residue ref kept" yes "$(exists "$R" "$REF")"

  echo "c14h residue taken on an unlanded commit whose branch was then deleted (R^1 held by nothing)"
  gq "$R" checkout -q -b parked; put "$R" u.txt 'unlanded\n'; U="$(cm "$R" U)"
  REF="$(residue "$R" 20260901T000008Z f.txt 'second\r\n')"
  gq "$R" checkout -q main; gq "$R" branch -D parked
  QONTINUI_RUNNER_REPO="$NORUN" reap "$R" "$REF"
  outcome_is "c14h" 2 REFUSED
  eq "c14h: residue_base failed" fail "$(chk residue_base)"
  has "c14h: reason names the base commit" "only ref holding its base commit $U" "$(jf reason)"
  eq "c14h: residue ref kept" yes "$(exists "$R" "$REF")"

  echo "c14i the same residue while a local branch still holds R^1"
  gq "$R" checkout -q -b parked2; put "$R" u2.txt 'unlanded two\n'; cm "$R" U2 >/dev/null
  REF="$(residue "$R" 20260901T000010Z f.txt 'second\r\n')"
  gq "$R" checkout -q main
  QONTINUI_RUNNER_REPO="$NORUN" reap "$R" "$REF"
  outcome_is "c14i" 0 REAPED
  has "c14i: residue_base names the local branch" "refs/heads/parked2 contains R^1" "$OUT"
  eq "c14i: residue ref deleted" no "$(exists "$R" "$REF")"

  echo "c14f no origin/<default>"
  C="$SANDBOX/c14f"; R="$C/c14f"; mkdir -p "$R"
  git init -q --bare "$(nw "$C/origin.git")"; git init -q "$(nw "$R")"
  gq "$R" remote add origin "$(nw "$C/origin.git")"
  put "$R" f.txt 'one\n'; cm "$R" v1 >/dev/null
  gq "$R" push -q origin main:refs/heads/trunk
  gq "$R" update-ref -d refs/remotes/origin/main 2>/dev/null
  REF="$(residue "$R" 20260901T000007Z f.txt 'two\n')"
  QONTINUI_RUNNER_REPO="$NORUN" reap "$R" "$REF"
  outcome_is "c14f" 3 UNKNOWN
  has "c14f: reason names the missing default" "no origin/<default>" "$(jf reason)"
  eq "c14f: residue ref kept" yes "$(exists "$R" "$REF")"
fi

if want c15; then
  newcase c15
  echo "c15a ABSENT"
  reap "$R" "refs/wip/return-to-main/$STAMP-c15-abcdef1"
  outcome_is "c15a" 6 ABSENT

  echo "c15b malformed name"
  BADREF="refs/wip/return-to-main/not-a-stamped-leaf"
  gq "$R" update-ref --create-reflog -m "manual: garbage" "$BADREF" "$BASE"
  reap "$R" "$BADREF"
  outcome_is "c15b" 2 REFUSED
  has "c15b: reason" "unparseable snapshot name" "$(jf reason)"
  eq "c15b: ref kept" yes "$(exists "$R" "$BADREF")"

  echo "c15c target does not start with the name's sha7"
  put "$R" m.txt 'mainline\n'; M1="$(cm "$R" m1)"; gq "$R" push -q origin main
  MISREF="refs/wip/return-to-main/20260901T000009Z-c15-${M1:0:7}"
  gq "$R" update-ref --create-reflog -m "$(sweep_msg c15 feat)" "$MISREF" "$BASE"
  reap "$R" "$MISREF"
  outcome_is "c15c" 2 REFUSED
  has "c15c: reason" "moved off its recorded sha" "$(jf reason)"
  eq "c15c: ref kept" yes "$(exists "$R" "$MISREF")"

  echo "c15d default-branch fast-forward snapshot"
  REF="$(snap "$R" "$BASE" "$(sweep_msg c15 main)")"
  reap "$R" "$REF"
  outcome_is "c15d" 1 SNAPSHOT_ONLY
  has "c15d: reason names the default branch" "is the default branch main" "$(jf reason)"
  eq "c15d: refs/heads/main still exists" yes "$(exists "$R" refs/heads/main)"
  eq "c15d: snapshot deleted" no "$(exists "$R" "$REF")"
fi

if want c16; then
  echo "c16 usage"
  newcase c16
  OUT="$(QONTINUI_MACHINE_ID=dev-1 bash "$SUBJECT" "$R" "refs/wip/return-to-main/$STAMP-c16-abcdef1" 2>/dev/null)"; RC=$?
  eq "c16: missing --device exits 4" 4 "$RC"
  eq "c16: no decision line on a usage error" "" "$OUT"
  OUT="$(QONTINUI_MACHINE_ID=dev-1 bash "$SUBJECT" "$R" "refs/wip/return-to-main/$STAMP-c16-abcdef1" --device dev-1 --bogus 2>/dev/null)"; RC=$?
  eq "c16: unknown option exits 4" 4 "$RC"
fi

if want c17; then
  # Check 6 covers the ANCESTORS of reflog-named commits: the reflog names only
  # X2, but X2 sits on a unique X1 that no ref holds.
  echo "c17 reflog names X2 only; its unique parent X1 is reflog-only by ancestry"
  newcase c17
  put "$R" secret.txt 'unique secret work\n'; X1="$(cm "$R" "X1 unique")"
  put "$R" b.txt 'bbb\n'; X2="$(cm "$R" X2)"
  gq "$R" reset -q --hard "$BASE"      # main back to BASE; X1/X2 held by nothing yet
  gq "$R" branch feat "$X2"
  put "$R" b.txt 'bbb\n'; M="$(cm "$R" "X2 landed")"; gq "$R" push -q origin main
  gq "$R" branch -f feat main
  REF="$(snap "$R" "$M" "$(sweep_msg c17 feat)")"
  [ -z "$(g "$R" log -g --format=%H refs/heads/feat -- | grep -x "$X1")" ] || bad "c17: FIXTURE the reflog names X1 directly"
  reap "$R" "$REF"
  outcome_is "c17" 1 SNAPSHOT_ONLY
  eq "c17: check 6 failed" fail "$(chk 6_reflog_only_empty)"
  has "c17: X1 is the reflog-only commit named" "${X1:0:12}" "$(jf reason)"
  eq "c17: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
  eq "c17: X1 still reachable through feat's reflog" "$X1" "$(g "$R" rev-list $(g "$R" log -g --format=%H refs/heads/feat --) | grep -x "$X1")"
fi

if want c18; then
  # Patch equivalence is VERBATIM: a branch that indents c() OUT of the if block
  # is whitespace-only different from main, and `git cherry` calls it equal.
  echo "c18a whitespace-only divergence at the tip is not landed"
  newcase c18a
  TIP="$(branchcommit "$R" feat p.py 'if a:\n    b()\nc()\n' "feat py")"
  put "$R" p.py 'if a:\n    b()\n    c()\n'; cm "$R" "landed py (different semantics)" >/dev/null; gq "$R" push -q origin main
  eq "c18a: FIXTURE git cherry calls the tip equivalent" "- $TIP" "$(g "$R" cherry main feat)"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c18a feat)")"
  reap "$R" "$REF"
  outcome_is "c18a" 1 SNAPSHOT_ONLY
  eq "c18a: check 5 failed" fail "$(chk 5_landed)"
  eq "c18a: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
  eq "c18a: snapshot deleted (feat holds the sha)" no "$(exists "$R" "$REF")"

  echo "c18b the same whitespace-only divergence as a reflog-only commit"
  newcase c18b
  W="$(branchcommit "$R" feat p.py 'if a:\n    b()\nc()\n' "feat py")"
  put "$R" p.py 'if a:\n    b()\n    c()\n'; M="$(cm "$R" "landed py")"; gq "$R" push -q origin main
  gq "$R" branch -f feat main
  REF="$(snap "$R" "$M" "$(sweep_msg c18b feat)")"
  reap "$R" "$REF"
  outcome_is "c18b" 1 SNAPSHOT_ONLY
  eq "c18b: check 6 failed" fail "$(chk 6_reflog_only_empty)"
  has "c18b: W is the reflog-only commit named" "${W:0:12}" "$(jf reason)"
  eq "c18b: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
fi

if want c19; then
  echo "c19a a linked worktree is mid rebase -i on the branch (worktree list reads it detached)"
  newcase c19a
  gq "$R" checkout -q -b feat; put "$R" f.txt 'f\n'; TIP="$(cm "$R" f)"; gq "$R" checkout -q main
  push_to_main "$R" "$TIP"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c19a feat)")"
  gq "$R" worktree add -q "$(nw "$C/wt")" feat
  ( cd "$C/wt" && GIT_SEQUENCE_EDITOR="sed -i s/^pick/edit/" git rebase -q -i HEAD~1 >/dev/null 2>&1 )
  has "c19a: FIXTURE the linked worktree is mid-rebase" "rebase-merge" "$(ls "$(g "$R" rev-parse --path-format=absolute --git-common-dir)/worktrees/wt" 2>/dev/null | tr '\n' ' ')"
  reap "$R" "$REF"
  outcome_is "c19a" 1 SNAPSHOT_ONLY
  eq "c19a: check 3 failed" fail "$(chk 3_not_checked_out)"
  has "c19a: reason names the rebase" "mid-rebase" "$(jf reason)"
  eq "c19a: branch feat kept" yes "$(exists "$R" refs/heads/feat)"

  echo "c19b the main worktree is mid-bisect started on the branch"
  newcase c19b
  gq "$R" checkout -q -b feat
  for i in 1 2 3; do put "$R" f.txt "f$i\\n"; cm "$R" "f$i" >/dev/null; done
  TIP="$(g "$R" rev-parse HEAD)"; gq "$R" checkout -q main; push_to_main "$R" "$TIP"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c19b feat)")"
  gq "$R" checkout -q feat; gq "$R" bisect start feat "$BASE"
  eq "c19b: FIXTURE bisect detached HEAD" "" "$(g "$R" symbolic-ref -q HEAD)"
  reap "$R" "$REF"
  outcome_is "c19b" 1 SNAPSHOT_ONLY
  eq "c19b: check 3 failed" fail "$(chk 3_not_checked_out)"
  has "c19b: reason names the bisect" "mid-bisect" "$(jf reason)"
  eq "c19b: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
fi

if want c20; then
  echo "c20 empty snapshot message, unique landed tip match: never REAPED"
  newcase c20
  TIP="$(branchcommit "$R" feat f.txt 'f1\n' f1)"; push_to_main "$R" "$TIP"
  REF="refs/wip/return-to-main/$STAMP-c20-${TIP:0:7}"
  gq "$R" update-ref "$REF" "$TIP"
  reap "$R" "$REF"
  outcome_is "c20" 1 SNAPSHOT_ONLY
  eq "c20: message empty" "" "$(jf message)"
  eq "c20: branch_source" tip_match "$(jf branch_source)"
  has "c20: reason names the missing writer message" "carries no writer message" "$(jf reason)"
  eq "c20: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
  eq "c20: snapshot deleted (origin/main holds the sha)" no "$(exists "$R" "$REF")"
fi

if want c21; then
  echo "c21a check 3 re-probed before the delete: a linked worktree checks the branch out mid-run"
  newcase c21a
  TIP="$(branchcommit "$R" feat f.txt 'f1\n' f1)"; push_to_main "$R" "$TIP"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c21a feat)")"
  gq "$R" worktree add -q --detach "$(nw "$C/wt")" "$BASE"
  REAP_TEST_SEAMS=1 REAP_RACE_HOOK="git -C '$(nw "$C/wt")' checkout -q feat" reap "$R" "$REF"
  outcome_is "c21a" 2 REFUSED
  eq "c21a: check 3 passed first" pass "$(chk 3_not_checked_out)"
  eq "c21a: the re-check failed" fail "$(chk 3_recheck)"
  eq "c21a: deleted[] empty" '"deleted":[]' "$(printf '%s' "$OUT" | grep -o '"deleted":\[[^]]*\]')"
  eq "c21a: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
  eq "c21a: snapshot kept" yes "$(exists "$R" "$REF")"

  echo "c21b the same race via a bisect started on the branch"
  newcase c21b
  gq "$R" checkout -q -b feat
  for i in 1 2 3; do put "$R" f.txt "f$i\\n"; cm "$R" "f$i" >/dev/null; done
  TIP="$(g "$R" rev-parse HEAD)"; gq "$R" checkout -q main; push_to_main "$R" "$TIP"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c21b feat)")"
  gq "$R" worktree add -q --detach "$(nw "$C/wt")" "$BASE"
  REAP_TEST_SEAMS=1 REAP_RACE_HOOK="git -C '$(nw "$C/wt")' checkout -q feat && git -C '$(nw "$C/wt")' bisect start feat $BASE >/dev/null 2>&1" reap "$R" "$REF"
  outcome_is "c21b" 2 REFUSED
  eq "c21b: the re-check failed" fail "$(chk 3_recheck)"
  has "c21b: reason names the bisect" "mid-bisect" "$(jf reason)"
  eq "c21b: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
  eq "c21b: snapshot kept" yes "$(exists "$R" "$REF")"
fi

# --- round 3: fault-injecting git shims (resolved from THIS box's git, so the
# suite runs on Linux too) and a lossy textconv driver ---------------------------
REAL_GIT="$(command -v git)"
mkdir -p "$SANDBOX/shim-revlist" "$SANDBOX/shim-patchid"
cat >"$SANDBOX/shim-revlist/git" <<EOF
#!/bin/bash
# fails \`rev-list origin/main..<x>\` (not --count): check 5b's range read
case " \$* " in *" rev-list origin/main.."*) case " \$* " in *--count*) ;; *) echo "fatal: simulated rev-list failure" >&2; exit 128 ;; esac ;; esac
exec "$REAL_GIT" "\$@"
EOF
cat >"$SANDBOX/shim-patchid/git" <<EOF
#!/bin/bash
# fails \`patch-id\` on any non-empty input (the subject's empty capability probe still passes)
if [ "\$1" = patch-id ]; then d="\$(cat)"; [ -z "\$d" ] && exit 0; echo "fatal: simulated patch-id failure" >&2; exit 128; fi
exec "$REAL_GIT" "\$@"
EOF
cat >"$SANDBOX/bin/lossy.sh" <<'EOF'
#!/bin/sh
tr -d '0-9' < "$1"
EOF
cat >"$SANDBOX/bin/edit-t.sh" <<'EOF'
#!/bin/sh
awk '{ l[NR]=$0 } END { for (i=1;i<=NR;i++) { if (l[i] ~ /^pick .* t$/) sub(/^pick/, "edit", l[i]); print l[i] } }' "$1" > "$1.new" && mv "$1.new" "$1"
EOF
chmod +x "$SANDBOX/shim-revlist/git" "$SANDBOX/shim-patchid/git" "$SANDBOX/bin/lossy.sh" "$SANDBOX/bin/edit-t.sh"
SHIM_REVLIST="$SANDBOX/shim-revlist:$PATH"
SHIM_PATCHID="$SANDBOX/shim-patchid:$PATH"

if want c22; then
  echo "c22a a lossy textconv makes a reflog-only commit look landed (check 6)"
  newcase c22a
  gq "$R" config diff.lossy.textconv "$SANDBOX/bin/lossy.sh"
  put "$R" .gitattributes '*.cfg diff=lossy\n'; put "$R" app.cfg 'mode=x\n'; cm "$R" cfg >/dev/null; gq "$R" push -q origin main
  gq "$R" checkout -q -b tmpx; put "$R" app.cfg 'mode=y1\n'; X="$(cm "$R" "set port")"; gq "$R" checkout -q main; gq "$R" branch -D tmpx
  gq "$R" branch feat "$X"
  put "$R" app.cfg 'mode=y2\n'; M="$(cm "$R" "set port")"; gq "$R" push -q origin main
  gq "$R" branch -f feat main
  has "c22a: FIXTURE the textconv hides the digit" "+mode=y" "$(g "$R" show "$X" | tail -1)"
  REF="$(snap "$R" "$M" "$(sweep_msg c22a feat)")"
  reap "$R" "$REF"
  outcome_is "c22a" 1 SNAPSHOT_ONLY
  eq "c22a: check 6 failed" fail "$(chk 6_reflog_only_empty)"
  has "c22a: X is the reflog-only commit named" "${X:0:12}" "$(jf reason)"
  eq "c22a: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
  eq "c22a: X still reachable through feat's reflog" "$X" "$(g "$R" log -g --format=%H refs/heads/feat -- | grep -x "$X")"

  # In the 5b arm the same content is already refused by the `git cherry`
  # pre-filter (patch-ids do not textconv): measured, and asserted below, so
  # this case is coverage of the arm, not of --no-textconv.
  echo "c22b the same lossy textconv at the tip (check 5)"
  newcase c22b
  gq "$R" config diff.lossy.textconv "$SANDBOX/bin/lossy.sh"
  put "$R" .gitattributes '*.cfg diff=lossy\n'; put "$R" app.cfg 'mode=x\n'; cm "$R" cfg >/dev/null; gq "$R" push -q origin main
  TIP="$(branchcommit "$R" feat app.cfg 'mode=y1\n' "set port")"
  put "$R" app.cfg 'mode=y2\n'; cm "$R" "set port" >/dev/null; gq "$R" push -q origin main
  eq "c22b: FIXTURE git cherry's patch-id sees the difference" "+ $TIP" "$(g "$R" cherry main feat)"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c22b feat)")"
  reap "$R" "$REF"
  outcome_is "c22b" 1 SNAPSHOT_ONLY
  eq "c22b: check 5 failed" fail "$(chk 5_landed)"
  has "c22b: reason says not landed" "has not landed on origin/main" "$(jf reason)"
  eq "c22b: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
fi

if want c23; then
  echo "c23 check 5b's range read fails (content landed as a cherry-pick)"
  newcase c23
  TIP="$(branchcommit "$R" feat f.txt 'feature\n' "feat: f")"
  put "$R" m.txt 'other work\n'; cm "$R" m1 >/dev/null
  gq "$R" cherry-pick "$TIP"; gq "$R" push -q origin main
  REF="$(snap "$R" "$TIP" "$(sweep_msg c23 feat)")"
  PATH="$SHIM_REVLIST" reap "$R" "$REF"
  outcome_is "c23" 3 UNKNOWN
  eq "c23: check 5 unknown" unknown "$(chk 5_landed)"
  eq "c23: reason names the failed range read" "rev-list origin/main..$TIP failed" "$(jf reason)"
  eq "c23: deleted[] empty" '"deleted":[]' "$(printf '%s' "$OUT" | grep -o '"deleted":\[[^]]*\]')"
  eq "c23: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
  eq "c23: snapshot kept" yes "$(exists "$R" "$REF")"
  reap "$R" "$REF"
  outcome_is "c23 control: the same fixture without the shim" 0 REAPED
fi

if want c24; then
  echo "c24a patch-id fails on a rebased branch's reflog-only commit (check 6)"
  newcase c24a
  branchcommit "$R" feat f.txt 'feature\n' f1 >/dev/null
  put "$R" m.txt 'mainline\n'; cm "$R" m1 >/dev/null; gq "$R" push -q origin main
  gq "$R" checkout -q feat; gq "$R" rebase -q main; TIP="$(g "$R" rev-parse HEAD)"; gq "$R" checkout -q main
  push_to_main "$R" "$TIP"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c24a feat)")"
  PATH="$SHIM_PATCHID" reap "$R" "$REF"
  outcome_is "c24a" 3 UNKNOWN
  eq "c24a: check 6 unknown" unknown "$(chk 6_reflog_only_empty)"
  has "c24a: reason names the patch-id" "verbatim patch-id for reflog-only commit" "$(jf reason)"
  eq "c24a: deleted[] empty" '"deleted":[]' "$(printf '%s' "$OUT" | grep -o '"deleted":\[[^]]*\]')"
  eq "c24a: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
  eq "c24a: snapshot kept" yes "$(exists "$R" "$REF")"
  reap "$R" "$REF"
  outcome_is "c24a control: the same fixture without the shim" 0 REAPED

  echo "c24b patch-id fails in the 5b arm (content landed as a cherry-pick)"
  newcase c24b
  TIP="$(branchcommit "$R" feat f.txt 'feature\n' "feat: f")"
  put "$R" m.txt 'other work\n'; cm "$R" m1 >/dev/null
  gq "$R" cherry-pick "$TIP"; gq "$R" push -q origin main
  REF="$(snap "$R" "$TIP" "$(sweep_msg c24b feat)")"
  PATH="$SHIM_PATCHID" reap "$R" "$REF"
  outcome_is "c24b" 3 UNKNOWN
  eq "c24b: check 5 unknown" unknown "$(chk 5_landed)"
  eq "c24b: reason names the patch-id" "verbatim patch-id for $TIP could not be established" "$(jf reason)"
  eq "c24b: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
  eq "c24b: snapshot kept" yes "$(exists "$R" "$REF")"
  reap "$R" "$REF"
  outcome_is "c24b control: the same fixture without the shim" 0 REAPED
fi

if want c25; then
  echo "c25 a linked worktree runs rebase -i --update-refs on a branch stacked on feat"
  newcase c25
  gq "$R" checkout -q -b feat; put "$R" f.txt 'f\n'; TIP="$(cm "$R" f)"; gq "$R" checkout -q main
  push_to_main "$R" "$TIP"
  REF="$(snap "$R" "$TIP" "$(sweep_msg c25 feat)")"
  gq "$R" branch top feat; gq "$R" worktree add -q "$(nw "$C/wt")" top
  ( cd "$C/wt" && printf 't\n' >t.txt && git add t.txt && git commit -qm t \
      && GIT_SEQUENCE_EDITOR="$SANDBOX/bin/edit-t.sh" git rebase -i --no-ff --update-refs HEAD~2 >/dev/null 2>&1 )
  WTGIT="$(g "$R" rev-parse --path-format=absolute --git-common-dir)/worktrees/wt"
  has "c25: FIXTURE the rebase lists feat in update-refs" "refs/heads/feat" "$(cat "$WTGIT/rebase-merge/update-refs" 2>/dev/null)"
  eq "c25: FIXTURE the rebase's head-name is top, not feat" "refs/heads/top" "$(tr -d '\r\n' <"$WTGIT/rebase-merge/head-name" 2>/dev/null)"
  eq "c25: FIXTURE feat has not moved yet" "$TIP" "$(g "$R" rev-parse --verify --quiet refs/heads/feat)"
  reap "$R" "$REF"
  outcome_is "c25" 1 SNAPSHOT_ONLY
  eq "c25: check 3 failed" fail "$(chk 3_not_checked_out)"
  has "c25: reason names the update-refs" "listed in a rebase --update-refs" "$(jf reason)"
  eq "c25: branch feat kept" yes "$(exists "$R" refs/heads/feat)"
fi

# ================================================================================
# Mutation control. Each declared mutant is re-run against only the cases that
# pin its property; the control runs the whole suite against an unmutated copy
# staged alone, the same way the mutants are.
if ! mc_is_mutant && { [ -z "${REAP_TEST_ONLY:-}" ] || [ "${REAP_MUTATION_ONLY:-}" = 1 ]; }; then
  echo "-- mutation control"
  mwant() { [ -z "${REAP_MUTANT_ONLY:-}" ] && return 0; case " $REAP_MUTANT_ONLY " in *" $1 "*) return 0 ;; esac; return 1; }
  mx() { mwant "${1%% *}" || return 0; mc_expect_red "$@"; }
  mkdir -p "$SANDBOX/control"
  cp "$REAL_SUBJECT" "$SANDBOX/control/reap-restore-snapshot.sh"
  if ! mwant M0; then :
  elif env -u REAP_MUTATION_ONLY -u REAP_TEST_ONLY MC_MUTANT=1 MC_SUBJECT="$SANDBOX/control/reap-restore-snapshot.sh" bash "$SELF" >"$SANDBOX/control.log" 2>&1; then
    ok "M0 control: an unmutated copy staged alone passes the whole suite"
  else
    bad "M0 control: an UNMUTATED staged copy fails -- every red below could be the staging's" "$(tail -5 "$SANDBOX/control.log")"
  fi
  mc_ok()  { ok "$*"; }
  mc_bad() { bad "$*"; }
  mx "Ma check 6 loses its patch-equivalence exclusion" "$REAL_SUBJECT" \
    's/if \[ "\$_vr" = 0 \]; then _equiv=/if false; then _equiv=/' \
    -- env -u REAP_MUTATION_ONLY REAP_TEST_ONLY="c04" bash "$SELF"
  mx "Mb check 4 (tip unchanged) always passes" "$REAL_SUBJECT" \
    's/if \[ "\$TIP" != "\$SNAP_SHA" \]; then/if false; then/' \
    -- env -u REAP_MUTATION_ONLY REAP_TEST_ONLY="c06" bash "$SELF"
  mx "Mc the device check is removed" "$REAL_SUBJECT" \
    's/^  finish DEFERRED "wrong_device: .*$/  :/' \
    -- env -u REAP_MUTATION_ONLY REAP_TEST_ONLY="c10" bash "$SELF"
  mx "Md the branch delete is not compare-and-delete" "$REAL_SUBJECT" \
    's|gw update-ref -d "refs/heads/\$BRANCH" "\$SNAP_SHA"|gw update-ref -d "refs/heads/$BRANCH"|' \
    -- env -u REAP_MUTATION_ONLY REAP_TEST_ONLY="c09" bash "$SELF"
  mx "Me residue: the R^2^{tree} == R^1^{tree} check is dropped" "$REAL_SUBJECT" \
    's/^    finish REFUSED "residue snapshot carries staged changes"$/    :/' \
    -- env -u REAP_MUTATION_ONLY REAP_TEST_ONLY="c14" bash "$SELF"
  # Mf keeps git's reachability filter and intersects its output with the named
  # reflog shas: the ancestors are dropped and nothing else changes. (Adding
  # `rev-list --no-walk` is NOT that mutant: git walks anyway once negatives are
  # given, which is why the first draft of Mf stayed green.)
  mx "Mf check 6 drops ancestors (the named reflog shas alone)" "$REAL_SUBJECT" \
    's|--exclude="\$WIP_REF" --all >"\$TMP/only" 2>/dev/null; then$|--exclude="$WIP_REF" --all >"$TMP/only0" 2>/dev/null; then :; fi; printf "%s\\n" "${_named[@]}" \| grep -xFf - "$TMP/only0" >"$TMP/only"; if false; then|' \
    -- env -u REAP_MUTATION_ONLY REAP_TEST_ONLY="c17" bash "$SELF"
  mx "Mg verbatim_landed compares with a plain git cherry match" "$REAL_SUBJECT" \
    '/^  \[ -n "\$PATCHID_VERBATIM" \] || return 2$/a\  [ "$(g cherry "$DEFAULT_REF" "$c" "$c^" 2>/dev/null)" = "- $c" ]; return $?' \
    -- env -u REAP_MUTATION_ONLY REAP_TEST_ONLY="c18" bash "$SELF"
  mx "Mh check 3 loses the rebase-merge/head-name probe" "$REAL_SUBJECT" \
    's|for f in rebase-merge/head-name rebase-apply/head-name |for f in rebase-apply/head-name |' \
    -- env -u REAP_MUTATION_ONLY REAP_TEST_ONLY="c19" bash "$SELF"
  mx "Mi --no-textconv is dropped from VERBATIM_DIFF_OPTS" "$REAL_SUBJECT" \
    's|^VERBATIM_DIFF_OPTS=(--no-color --no-ext-diff --no-textconv |VERBATIM_DIFF_OPTS=(--no-color --no-ext-diff |' \
    -- env -u REAP_MUTATION_ONLY REAP_TEST_ONLY="c22" bash "$SELF"
  mx "Mj check 5b ignores the range rev-list's exit code" "$REAL_SUBJECT" \
    's|if ! g rev-list "\$DEFAULT_REF\.\.\$TIP" >"\$TMP/5b" 2>/dev/null; then|if ! { g rev-list "$DEFAULT_REF..$TIP" >"$TMP/5b" 2>/dev/null; true; }; then|' \
    -- env -u REAP_MUTATION_ONLY REAP_TEST_ONLY="c23" bash "$SELF"
  mx "Mk verbatim_landed's patch-id failure is 'not equal' again (return 1)" "$REAL_SUBJECT" \
    '/git patch-id --verbatim <"\$TMP\/v[cl]"/s/|| return 2$/|| return 1/' \
    -- env -u REAP_MUTATION_ONLY REAP_TEST_ONLY="c24" bash "$SELF"
  mx "Ml check 3 loses the rebase --update-refs probe" "$REAL_SUBJECT" \
    's| BISECT_START rebase-merge/update-refs; do| BISECT_START; do|' \
    -- env -u REAP_MUTATION_ONLY REAP_TEST_ONLY="c25" bash "$SELF"
  if [ "$MC_RED" -lt "$MC_DECLARED" ]; then
    bad "mutation control: $MC_RED of $MC_DECLARED declared mutation(s) reddened the suite"
  fi
fi

if [ $((PASS + FAIL)) -eq 0 ]; then
  echo "reap-restore-snapshot-test: NO ASSERTIONS RAN (REAP_TEST_ONLY='${REAP_TEST_ONLY:-}') -- refusing to report green" >&2
  exit 1
fi
echo "reap-restore-snapshot-test: $PASS passed, $FAIL failed"
mc_is_mutant || mc_trailer "$((PASS + FAIL))"
[ "$FAIL" -eq 0 ]
