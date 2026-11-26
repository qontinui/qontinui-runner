#!/usr/bin/env bash
# Self-test for /coord-revive's APPROVAL half -- the reporter that names the
# second way a coord_* tool goes missing.
#
# WHY THIS SUITE EXISTS. `.mcp.json` DECLARES the coord-mcp server; a settings
# key APPROVES it, and Claude Code will not load a project-scoped server it has
# not approved. Before this reporter the cascade could probe a door, find it
# LIVE, and tell an agent to re-issue over it while the agent had no coord tools
# at all -- the client's mask wearing a success label. PR #370 restored the
# approval key and closed naming exactly that gap.
#
# WHAT IT ASSERTS, and the shape of every assertion: a typed summary token must
# be reachable, and -- the half that actually matters -- must NOT be reachable
# from an input that merely resembles it. Every positive case below is paired
# with a negative control, because a classifier that only ever answers "yes" is
# indistinguishable from a broken one that cannot answer "no".
#
# THE VERDICT INVARIANT. The approval half is a different transport from every
# door the cascade probes, so it must never move the VERDICT line or the exit
# code. Case 9 asserts that directly: two runs whose ONLY difference is the
# approval fixture must agree on both.
#
# THE ROSTER INVARIANT (case 11). Reachability and documentedness are different
# properties, and only the first is testable by running fixtures. Case 11 diffs
# the tokens the script can assign against the rows of the SKILL.md summary
# table, in both directions -- the table being what an agent greps when it meets
# an APPROVAL: line it does not recognise. It is here rather than in a lint
# check because the two files it compares are this skill's own, and a suite that
# already runs the script is the cheapest place to notice they disagree.
#
# THE DISCHARGE. The origin sentinel is shown able to fail in-suite: one
# mutation of a staged copy of coord-revive.sh -- L6 ignoring its override --
# must redden a re-run, behind an unmutated copy staged the same way that must
# run green. Every copy sits exactly three directories below the sandbox,
# whose own scripts link is where the subject's resolver looks first; the
# helper's STAGED COPIES says why the depth matters. Both re-runs are
# narrowed with `--only configdir` to one of the suite's five sections
# (classify, invariant, worktree, configdir, roster), so the discharge costs
# four subject runs rather than two whole suites.
#
# ISOLATION, and it is load-bearing rather than tidy. Every run of the subject
# is `env -i` with an explicit allow-list: the shared PINNED_ENV that
# scripts/lib/coord-revive-test-isolation.sh builds -- its header says what it
# carries and what it keeps out -- plus the five each invocation sets for
# itself (its $PWD, its home and user profile, its Claude config dir and its
# workspace root) and, in inv_run alone, the four knobs that pin the floor
# gate open so its two runs agree (COORD_REVIVE_LOAD_RATIO,
# COORD_REVIVE_BUILD_PROCS, COORD_REVIVE_HEALTH_SLOW_MS,
# COORD_REVIVE_LOADAVG_FILE). So the subject sees
# those variables and nothing else the caller exports. That replaced pinning
# each risk by name, which missed five, each measured while every assertion
# here passed: the web host (below); the runner port $QONTINUI_RUNNER_PORT,
# which the frontend-state helper appends after the origins it is given (20
# /health reads a run reached an exported port); the write paths the cascade
# honours AHEAD of $HOME ($COORD_REVIVE_TIMING_FILE,
# $QONTINUI_GUARD_DECISION_LOG and the $QONTINUI_LOG_DIR it defaults under,
# $CAPNEG_STATE_DIR and $QONTINUI_RUNG_UNREPORTED_LOG -- a run appended to
# three of the caller's files, the timing file, the guard-decision log and the
# capability-negative ledger, a sandbox FLOOR-CLAIM row into the last each
# time); the caller's curl configuration,
# which every dial obeyed; and $BASH_ENV, which bash sourced into the subject.
# Credentials, session ids, proxy variables and every $COORD_REVIVE_* knob the
# caller may have tuned stay out the same way. The same helper's origin
# sentinel then reads every run and fails one that names an origin other than
# the dead port, so a pin that stops holding shows as a red case rather than
# as traffic nobody saw. Case 10 runs git itself, in the suite's environment
# rather than the subject's, so its fixture drops the caller's git variables on
# its own (fixture_git_env, beside it).
# Every case also runs in its own temp tree with $HOME redirected there, so
# the suite never reads or writes the real user store and a machine whose own
# trust state changes cannot flip a result. These pins matter as much:
#
#   $CLAUDE_CONFIG_DIR    emptied, and this one was NOT optional. It relocates
#                         BOTH the user settings file and the user store, and it
#                         is set on the operator box
#                         (C:\claude\.claude-tiohorst). While coord-revive.sh
#                         ignored it, redirecting $HOME was enough; the moment it
#                         was taught to honour it -- the fix in the same change
#                         as this line -- an unpinned run would classify the
#                         OPERATOR's live .claude.json, 12 real projects and all.
#                         So the previous hermeticity claim was true only by
#                         virtue of the bug it sat next to.
#
#   $QONTINUI_ROOT        pins L2's sibling sweep to the sandbox. WITHOUT it the
#                         sweep resolves through resolve_root()'s $HERE fallback
#                         -- this file lives inside the repo -- reaches the REAL
#                         workspace root, and probes every sibling's .mcp.json
#                         against the operator's live runner. Measured before it
#                         was set: 3m18s for ONE case, and answers that depended
#                         on whether a runner happened to be up. That is not a
#                         slow test, it is a test of the wrong thing.
#   $QONTINUI_RUNNER_URL  (shared list) points L4's mint at the dead port.
#   $COORD_HTTP_URL       (shared list) points L3/L4's bearer doors at the dead
#                         port, so a credential that somehow resolves cannot
#                         reach coord.
#   $QONTINUI_WEB_HTTP_URL (shared list) points L6 -- the WEB host, a second
#                         remote host with its own variable -- at the dead port.
#                         $COORD_HTTP_URL does not reach it: from the day L6
#                         landed (qontinui-claude-config#888) until it was
#                         pinned, every case sent an anonymous GET to production
#                         api.qontinui.io while this header claimed hermeticity.
# The shared list's other entries -- $PATH, the caller's and the one channel
# for code that env -i leaves open, by design; $CURL_HOME at an empty
# .curlrc; the sandbox $TMPDIR, $TEMP and $TMP; and the Windows system
# variables this suite needs on the windows leg -- are explained in the
# helper's own header.
#
# The RUNNER_URL line is belt; the COORD_HTTP_URL line is braces, and the braces
# are there because the belt silently broke once. Until 2026-08-29 coord-revive
# probed `http://127.0.0.1:9876` unconditionally -- $QONTINUI_RUNNER_URL
# PREPENDED rather than overrode -- so this suite minted a real Cognito access
# token from the operator's live runner and sent it to production coord on every
# case, 13 times a run, while its own header claimed hermeticity. Invisible on a
# CI box with nothing on 9876. The script was fixed to honour the override it
# documents; this second variable means a regression there costs a failed probe
# rather than live traffic. A test harness that reaches production is worse than
# no harness, so it is pinned in BOTH places rather than in the one that is
# currently sufficient.

set -u

# -P resolves symlinks. Run through a workspace root's .claude symlink, a
# logical cd would climb ../../.. on the symlink's side and miss scripts/lib.
HERE="$(cd -P "$(dirname "$0")" && pwd)"
# MC_SUBJECT is the discharge's seam: a re-run is pointed at a staged copy.
SCRIPT="${MC_SUBJECT:-$HERE/coord-revive.sh}"
REPO_ROOT="$(cd -P "$HERE/../../.." && pwd)"
PASS=0
FAIL=0
FAILED_CASES=()

[ -r "$SCRIPT" ] || { echo "FATAL: $SCRIPT not readable"; exit 1; }
# The pinned environment and the origin sentinel, shared with
# floor-claim-test.sh. Without it the subject cannot be isolated, so the suite
# refuses rather than run it.
ISOLATION_LIB="$REPO_ROOT/scripts/lib/coord-revive-test-isolation.sh"
# shellcheck source=/dev/null
. "$ISOLATION_LIB" || { echo "FATAL: cannot source $ISOLATION_LIB (resolved from HERE=$HERE), so the subject cannot be isolated"; exit 1; }

# ok / bad: one assertion each, in this suite's own line format. The shared
# origin sentinel (check_origins) reports through them.
ok()  { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad() { FAIL=$((FAIL + 1)); FAILED_CASES+=("$1"); echo "  FAIL $1"; }

# mutation-control.sh, from the same checkout: this suite's sections and its
# discharge. Each block below runs only when its section is selected; a run
# given `--only <id>[,<id>]` runs those alone, and `--list-sections` prints the
# vocabulary. The runner always runs the suite whole.
MC_LIB="$REPO_ROOT/scripts/lib/mutation-control.sh"
# shellcheck source=/dev/null
. "$MC_LIB" || { echo "FATAL: cannot source $MC_LIB (resolved from HERE=$HERE), so this suite's sections and discharge cannot run"; exit 1; }
mc_ok()  { ok "$*"; }
mc_bad() { bad "$*"; }
mc_only_declare classify invariant worktree configdir roster
mc_only_parse "$@"

# Sandbox root for the whole suite. Named loudly: a mktemp failure here would
# otherwise surface as $HOME pointing at an empty string, and the suite would
# then read the REAL ~/.claude.json -- a test that silently measures the
# operator's machine is worse than no test.
SANDBOX="$(mktemp -d)" || { echo "FATAL: mktemp -d failed"; exit 1; }
# Links come off first, never recursed through: see the helper's make_link.
trap 'isolation_unlink_all; rm -rf "$SANDBOX"' EXIT
# Where a staged lone copy's resolver looks first: the discharge stages every
# copy exactly three directories below $SANDBOX, so its rungs 1 and 2 land
# here. The helper's STAGED COPIES says what landed outside before.
make_link "$SANDBOX/scripts" "$REPO_ROOT/scripts"

# THE PINNED ENVIRONMENT -- with the five variables each invocation sets for
# itself (its $PWD, after the cd into its directory; home, user profile,
# Claude config dir, workspace root) and inv_run's four floor-gate knobs
# (COORD_REVIVE_LOAD_RATIO, COORD_REVIVE_BUILD_PROCS,
# COORD_REVIVE_HEALTH_SLOW_MS, COORD_REVIVE_LOADAVG_FILE), the WHOLE
# environment the subject sees: every run of it is `env -i` with these. Built
# by the shared helper (the header's ISOLATION says why); its origin sentinel
# pins the dead port alone here, and no run is exempt from naming one.
isolation_init "$SANDBOX"

CASE_N=0

# run_case <name> <expect-substring> <not-expect-substring|-> <setup-fn>
#
# The setup function receives the case's project dir as $1 and its fake $HOME as
# $2, and writes whatever fixtures the case needs. Both directories start empty.
#
# `%PROJ%` in either substring expands to this case's project dir, so an
# assertion can anchor on the LAYER PATH rather than on a bare sentence several
# readers emit. Written as a placeholder rather than a literal because the path
# contains the case ORDINAL -- spelling it out would silently re-point the
# assertion at another case's directory the first time a case is inserted above
# it, and a path that does not appear in the output can only fail, never pass
# for the wrong reason. It is substituted here, where the ordinal is known.
run_case() {
  local name="$1" expect="$2" reject="$3" setup="$4"
  CASE_N=$((CASE_N + 1))
  local dir="$SANDBOX/c$CASE_N"
  local home="$dir/home"
  local proj="$dir/proj"
  mkdir -p "$home" "$proj/.claude" || { echo "FATAL: mkdir failed for $name"; exit 1; }
  expect="${expect//%PROJ%/$proj}"
  reject="${reject//%PROJ%/$proj}"

  "$setup" "$proj" "$home"

  local out
  out="$(cd "$proj" && env -i "${PINNED_ENV[@]}" PWD="$PWD" HOME="$home" USERPROFILE="$home" \
        CLAUDE_CONFIG_DIR="" QONTINUI_ROOT="$proj" \
        bash "$SCRIPT" 2>&1)"

  local matched=1
  case "$out" in *"$expect"*) ;; *) matched=0 ;; esac
  if [ "$reject" != "-" ]; then
    case "$out" in *"$reject"*) matched=0 ;; esac
  fi

  if [ "$matched" = "1" ]; then
    ok "$name"
  else
    bad "$name"
    echo "       expected to contain: $expect"
    [ "$reject" = "-" ] || echo "       expected NOT to contain: $reject"
    echo "       ---- APPROVAL lines actually emitted ----"
    printf '%s\n' "$out" | grep "APPROVAL:" | sed 's/^/       /'
    echo "       ----------------------------------------"
  fi
  check_origins "$name" "$out"
}

decl_coord() { printf '%s' '{"mcpServers":{"coord-mcp":{}}}' > "$1/.mcp.json"; }

echo "coord-revive APPROVAL half:"

if mc_sec classify; then
# --- 1. REJECTED, and it outranks an approval sitting beside it ---------------
# disabledMcpjsonServers "still rejects the server, even in an untrusted folder"
# and in every permission mode, so it is the one reading here that is decisive on
# its own. The fixture deliberately ALSO approves the server: if the ordering
# were wrong this case would report APPROVED_TRUST_GATED and an agent would go
# hunting a transport fault that does not exist.
setup_rejected() {
  decl_coord "$1"
  printf '%s' '{"enabledMcpjsonServers":["coord-mcp"],"disabledMcpjsonServers":["coord-mcp"]}' \
    > "$1/.claude/settings.json"
}
run_case "REJECTED outranks a co-located approval" \
  "APPROVAL: REJECTED" "APPROVED" setup_rejected

# --- 1b. NEGATIVE CONTROL: a disable list that names something else -----------
# The rejection must key on the SERVER NAME, not on the key being present. A
# membership test that degenerated into a "is the array non-empty" test would
# pass case 1 and fail here.
setup_not_rejected() {
  decl_coord "$1"
  printf '%s' '{"enabledMcpjsonServers":["coord-mcp"],"disabledMcpjsonServers":["some-other-server"]}' \
    > "$1/.claude/settings.json"
}
run_case "a disable list naming another server does NOT reject" \
  "APPROVAL: APPROVAL_TRUST_UNKNOWN" "REJECTED" setup_not_rejected

# --- 2. NOT_APPLICABLE: nothing declared, so nothing to approve ---------------
# The missing half here is the DECLARATION, and saying "no approval found" would
# send the reader to the wrong half of the wiring.
setup_no_decl() {
  printf '%s' '{"mcpServers":{"something-else":{"url":"http://127.0.0.1:9/x"}}}' > "$1/.mcp.json"
  printf '%s' '{}' > "$1/.claude/settings.json"
}
run_case "no coord-mcp declared reports NOT_APPLICABLE" \
  "APPROVAL: NOT_APPLICABLE" "NO_APPROVAL_FOUND" setup_no_decl

# --- 3. APPROVED_UNGATED: the user layer is not gated on workspace trust ------
# An approval in ~/.claude/settings.json applies whether or not the folder is
# trusted, so reporting it as HELD would be wrong even though the folder here is
# untrusted (no ~/.claude.json is written at all).
setup_ungated() {
  decl_coord "$1"
  mkdir -p "$2/.claude"
  printf '%s' '{"enabledMcpjsonServers":["coord-mcp"]}' > "$2/.claude/settings.json"
}
run_case "user-layer approval reports APPROVED_UNGATED" \
  "APPROVAL: APPROVED_UNGATED" "HELD_UNTRUSTED" setup_ungated

# --- 4. APPROVED_TRUST_GATED: repo-supplied approval in a TRUSTED folder ------
# The project dir is not a git repository, so the documented trust key is the
# directory the session started from -- which is what the fixture writes.
#
# This case is ALSO the only regression guard on the MSYS env-conversion fix, and
# it is a PLATFORM-CONDITIONAL one -- said plainly rather than counted as
# coverage. `mktemp -d` yields a POSIX `/tmp/...` path under Git Bash, so the
# trust key crosses into native jq POSIX-spelled; without
# `MSYS2_ENV_CONV_EXCL` naming it, MSYS rewrites it to `C:/Users/.../Temp/...`,
# no `projects` entry matches, and this case reports APPROVAL_TRUST_UNKNOWN.
# That is how the bug was found. On Linux CI there is no conversion to defeat, so
# this case passes with or without the fix: a Windows run is what actually
# exercises it. Cases 5, 5c and 1b do NOT substitute -- all three expect
# TRUST_UNKNOWN, which is precisely what the bug produced unconditionally, so
# they went green while it was live.
setup_trusted() {
  decl_coord "$1"
  printf '%s' '{"enabledMcpjsonServers":["coord-mcp"]}' > "$1/.claude/settings.json"
  printf '{"projects":{"%s":{"hasTrustDialogAccepted":true}}}' "$1" > "$2/.claude.json"
}
run_case "repo approval in a trusted folder reports APPROVED_TRUST_GATED" \
  "APPROVAL: APPROVED_TRUST_GATED" "HELD_UNTRUSTED" setup_trusted

# --- 5. APPROVAL_HELD_UNTRUSTED, and `noentry` is not a refusal ---------------
# A folder Claude Code has no record of is UNKNOWN, not declined; the summary
# must still hold the repo-supplied approval, and the trust line must say
# "no projects entry" rather than reporting a refusal that never happened.
setup_untrusted() {
  decl_coord "$1"
  printf '%s' '{"enabledMcpjsonServers":["coord-mcp"]}' > "$1/.claude/settings.json"
  printf '%s' '{"projects":{"D:/somewhere/else":{"hasTrustDialogAccepted":true}}}' > "$2/.claude.json"
}
run_case "repo approval with no trust ENTRY reports APPROVAL_TRUST_UNKNOWN" \
  "APPROVAL: APPROVAL_TRUST_UNKNOWN" "APPROVAL_HELD_UNTRUSTED" setup_untrusted
run_case "an absent projects entry is reported as UNKNOWN, not a refusal" \
  "no projects entry at all" "NOT accepted" setup_untrusted

# --- 5b. ...and a RECORDED refusal is the one case that IS held ---------------
# The positive case for `declined`. Without it the suite only ever exercised the
# absent-entry path, and the summary could collapse "never visited" into
# "refused" -- which it did until pre-PR review, because every state that was
# not `accepted` fell into APPROVAL_HELD_UNTRUSTED. Two tokens, two fixtures.
setup_declined() {
  decl_coord "$1"
  printf '%s' '{"enabledMcpjsonServers":["coord-mcp"]}' > "$1/.claude/settings.json"
  printf '{"projects":{"%s":{"hasTrustDialogAccepted":false}}}' "$1" > "$2/.claude.json"
}
run_case "a recorded hasTrustDialogAccepted:false reports APPROVAL_HELD_UNTRUSTED" \
  "APPROVAL: APPROVAL_HELD_UNTRUSTED" "APPROVAL_TRUST_UNKNOWN" setup_declined

# --- 5c. NO .mcp.json AT ALL is DECLARATION_UNKNOWN, not a claim of one -------
# Every other fixture writes a .mcp.json, which is exactly how the original
# summary got away with asserting "coord-mcp is declared" for an absent file:
# the initialiser `unreadable` fell through to NO_APPROVAL_FOUND. Running from a
# directory with no .mcp.json is not exotic -- it is what the knowledge base
# tells an agent to do in the sibling repos.
setup_no_mcp_json() {
  printf '%s' '{"enabledMcpjsonServers":["coord-mcp"]}' > "$1/.claude/settings.json"
}
run_case "an absent .mcp.json reports DECLARATION_UNKNOWN" \
  "APPROVAL: DECLARATION_UNKNOWN" "is declared" setup_no_mcp_json

# --- 5d. enableAllProjectMcpServers is an approval in its own right -----------
# The `a = "true"` branch had no case at all: every fixture approved by NAME, so
# the blanket-flag arm was dead code in both readers.
setup_blanket() {
  decl_coord "$1"
  printf '%s' '{"enableAllProjectMcpServers":true}' > "$1/.claude/settings.json"
  printf '{"projects":{"%s":{"hasTrustDialogAccepted":true}}}' "$1" > "$2/.claude.json"
}
run_case "enableAllProjectMcpServers:true counts as an approval" \
  "APPROVAL: APPROVED_TRUST_GATED" "NO_APPROVAL_FOUND" setup_blanket

# --- 5e. a rejection in the USER layer rejects too ----------------------------
# disabledMcpjsonServers works "in any settings file". Only the project layer was
# tested, so the ungated/gated bookkeeping around rejection was unexercised.
setup_user_reject() {
  decl_coord "$1"
  mkdir -p "$2/.claude"
  printf '%s' '{"enabledMcpjsonServers":["coord-mcp"]}' > "$1/.claude/settings.json"
  printf '%s' '{"disabledMcpjsonServers":["coord-mcp"]}' > "$2/.claude/settings.json"
}
run_case "a disable entry in the USER layer rejects as well" \
  "APPROVAL: REJECTED" "APPROVED" setup_user_reject

# --- 6. NO_APPROVAL_FOUND, stated as UNKNOWN rather than as proof -------------
setup_none() {
  decl_coord "$1"
  printf '%s' '{"permissions":{"allow":[]}}' > "$1/.claude/settings.json"
}
run_case "declared with no approving key reports NO_APPROVAL_FOUND" \
  "APPROVAL: NO_APPROVAL_FOUND" "APPROVED" setup_none
run_case "NO_APPROVAL_FOUND is stated as UNKNOWN, not as proof" \
  "UNKNOWN rather than proof" "-" setup_none

# --- 7. NEGATIVE CONTROL on the reader: a wrong TYPE is not an approval -------
# `enabledMcpjsonServers` hand-edited to a string is a real mistake. It must
# report badtype and must NOT be read as naming the server -- a truthy-string
# test would approve it, which is the worst direction to be wrong in.
setup_badtype() {
  decl_coord "$1"
  printf '%s' '{"enabledMcpjsonServers":"coord-mcp"}' > "$1/.claude/settings.json"
}
run_case "a string enabledMcpjsonServers reports badtype, not an approval" \
  "enabledMcpjsonServers=badtype" "APPROVED" setup_badtype

# --- 8. NEGATIVE CONTROL: unparseable settings are UNKNOWN, not absent --------
setup_badjson() {
  decl_coord "$1"
  printf '%s' '{"enabledMcpjsonServers": [' > "$1/.claude/settings.json"
}
# Anchored on the LAYER and its path, not on the bare sentence. Two different
# readers emit "does not parse as JSON" -- the declaration reader and the
# settings reader -- and the settings reader maps any empty output to badjson,
# so the bare substring passes even if approval_keys were replaced by `true`.
run_case "unparseable settings report a parse failure, not an absent approval" \
  "project-shared %PROJ%/.claude/settings.json: does not parse as JSON" "-" setup_badjson

fi

if mc_sec invariant; then
# --- 9. THE VERDICT INVARIANT -------------------------------------------------
# Two runs differing ONLY in the approval fixture must produce the same VERDICT
# line and the same exit code. This is the property that makes the block safe to
# add to a tool whose DEAD line is sold as honest blocked-evidence.
#
# ONE DIRECTORY, RUN TWICE -- not two directories. The earlier cut gave each run
# its own sandbox (`inv/approved`, `inv/bare`), and the DEAD verdict line QUOTES
# the door paths it probed:
#   VERDICT: DEAD ... L1 (own /tmp/.../inv/approved/proj/.mcp.json), L2 (sibling
#   sweep under /tmp/.../inv/approved/proj), ...
# so the two strings differed in every run by construction and the assertion
# could never pass. Worse than merely broken: it failed claiming "the approval
# half moved the verdict" about a difference the approval half did not cause --
# a named-but-WRONG cause, in the one assertion that licenses adding this block
# to a tool whose DEAD line is sold as honest blocked-evidence.
#
# Masking the differing path out of the comparison would also work and is worse:
# it leaves the confound in and edits the evidence afterwards. Running the same
# directory twice removes the confound, and it is what the sentence above
# actually claims -- two runs whose ONLY difference is the approval fixture.
# The script only READS, so a second run over the same tree is well-defined.
echo "  -- verdict invariant --"
inv_dir="$SANDBOX/inv"
mkdir -p "$inv_dir/home" "$inv_dir/proj/.claude"
printf '0.00 0.00 0.00 1/1 1\n' > "$inv_dir/loadavg"   # the pinned load read (see inv_run)
decl_coord "$inv_dir/proj"
# THE LOAD GATE IS PINNED OPEN in both runs. coord-revive's floor gate
# (floor_gate_reason) turns a DEAD sweep's exit 1 into exit 5 whenever the box
# is under its own load: a load ratio, a count of live build processes (default
# threshold 0, so ONE cargo/rustc anywhere on the box trips it) or a slow
# /health. Those are sampled independently in each run, so on a busy CI runner
# the two runs could land on opposite sides of the threshold and this case
# failed "the approval half moved the exit code" about a difference the approval
# half did not cause -- measured on qontinui-claude-config#1183's windows shard
# 2026-09-26 (exit 1 with approval, 5 without; coord finding 7c45157e). The
# thresholds are pinned out of reach, and the load itself is read from a fixture
# (COORD_REVIVE_LOADAVG_FILE) rather than sampled, so the gate reads the same in
# both runs. One input stays sampled per run: the build-process census has no
# seam, and a census that FAILS (pgrep >= 2, tasklist error) still blocks FLOOR,
# so a failure in only one run could still flip the exit. That residual is left
# open deliberately -- it is a read failure, which the gate is right to report.
# Its stderr is kept apart, in <outfile>.err: the verdict comparison reads
# stdout alone, and the origin sentinel below reads both streams.
inv_run() {  # <outfile> -> prints "<exit> <verdict line>", stdout to <outfile>, stderr to <outfile>.err
  local o rc
  o="$(cd "$inv_dir/proj" && env -i "${PINNED_ENV[@]}" PWD="$PWD" \
       HOME="$inv_dir/home" USERPROFILE="$inv_dir/home" \
       CLAUDE_CONFIG_DIR="" QONTINUI_ROOT="$inv_dir/proj" \
       COORD_REVIVE_LOAD_RATIO=1000000 COORD_REVIVE_BUILD_PROCS=1000000 \
       COORD_REVIVE_HEALTH_SLOW_MS=1000000000 \
       COORD_REVIVE_LOADAVG_FILE="$inv_dir/loadavg" \
       bash "$SCRIPT" 2>"$1.err")"
  rc=$?
  printf '%s\n' "$o" > "$1"
  printf '%s %s' "$rc" "$(printf '%s\n' "$o" | grep -m1 '^VERDICT:')"
}
# Bare FIRST, then the identical tree with the approval added -- the only edit
# between the two runs.
inv_b="$(inv_run "$inv_dir/bare.out")"
printf '%s' '{"enabledMcpjsonServers":["coord-mcp"]}' > "$inv_dir/proj/.claude/settings.json"
inv_a="$(inv_run "$inv_dir/approved.out")"
# The origin sentinel over both runs and both streams, here rather than inside
# inv_run: that runs in a command substitution, where its count would be lost.
check_origins "verdict invariant, bare run" \
  "$(cat "$inv_dir/bare.out" 2>/dev/null)" "$(cat "$inv_dir/bare.out.err" 2>/dev/null)"
check_origins "verdict invariant, approved run" \
  "$(cat "$inv_dir/approved.out" 2>/dev/null)" "$(cat "$inv_dir/approved.out.err" 2>/dev/null)"
# THE WITNESS. Running one directory twice buys a clean comparison and costs the
# proof that the fixture did anything: if the settings write silently failed,
# both runs would be byte-identical and "the verdict did not move" would be
# vacuously true. So the APPROVAL summaries must DIFFER even as the verdicts
# agree -- that is the pair the invariant is actually about. (Bare reports
# NO_APPROVAL_FOUND; approved reports a trust-gated token, no ~/.claude.json
# being present here.) Without this the case is green by construction, which is
# the shape it already failed in once.
inv_tok_b="$(grep -m1 '^APPROVAL: ' "$inv_dir/bare.out" 2>/dev/null)"
inv_tok_a="$(grep -m1 '^APPROVAL: ' "$inv_dir/approved.out" 2>/dev/null)"
# The equality is not enough on its own. `printf '%s %s' "$rc" "$(grep ...)"`
# yields "1 " when grep matches NOTHING, so a `-n` test can never fail and two
# runs that both LOST their VERDICT line would compare equal and pass. Assert
# the line is actually there, then that the two agree.
if [ "$inv_a" = "$inv_b" ] \
   && case "$inv_a" in *"VERDICT:"*) true ;; *) false ;; esac \
   && [ -n "$inv_tok_a" ] && [ -n "$inv_tok_b" ] && [ "$inv_tok_a" != "$inv_tok_b" ]; then
  PASS=$((PASS + 1)); echo "  ok   the approval half changes neither the VERDICT line nor the exit code"
else
  FAIL=$((FAIL + 1)); FAILED_CASES+=("verdict invariant")
  echo "  FAIL the approval half moved the verdict or the exit code"
  echo "       with approval: $inv_a"
  echo "       without:       $inv_b"
  echo "       approval summary with:    ${inv_tok_a:-<none>}"
  echo "       approval summary without: ${inv_tok_b:-<none>}"
  [ "$inv_tok_a" = "$inv_tok_b" ] && echo "       (identical summaries -- the fixture did not take effect, so the comparison proved nothing)"
fi

fi

if mc_sec worktree; then
# --- 10. The trust key is the MAIN CHECKOUT's root, from a linked worktree ----
# The documented rule is that trust keys on the git repository root, and "in a
# worktree, it uses the main checkout's root". Reading the worktree's own path
# would look up a `projects` key that has never existed and report `noentry` for
# a repository whose trust was granted long ago -- so this builds a real repo and
# a real linked worktree rather than asserting the intent in prose.
echo "  -- worktree trust anchor --"
# The fixture's git runs in the suite's own environment, not the subject's
# pinned one, so an exported GIT_DIR -- or GIT_WORK_TREE, GIT_INDEX_FILE,
# GIT_COMMON_DIR, GIT_OBJECT_DIRECTORY, GIT_ALTERNATE_OBJECT_DIRECTORIES --
# would aim every command below at the CALLER's repository. Measured without
# this guard, with GIT_DIR naming a scratch repository: this case committed,
# branched, added a worktree and set config there, and every assertion still
# passed. So the fixture drops every exported GIT_* variable (one rule rather
# than a list that can drift) and reads neither the global nor the system
# config, through which a caller's core.hooksPath would run its hooks on the
# fixture's commit and a signing setting could fail that commit into the SKIP
# below; `git init --template=` copies no template hooks either. Call it in a
# subshell: it edits the environment of the shell that runs it.
fixture_git_env() {
  local v
  for v in $(compgen -e); do
    case "$v" in GIT_*) unset "$v" ;; esac
  done
  export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
  # GIT_CONFIG_GLOBAL needs git 2.32, and fleet Linux boxes carry older git
  # (coord-revive.sh records 2.25), which reads the global config from $HOME
  # and $XDG_CONFIG_HOME instead. Both point at an empty directory of the
  # fixture's own.
  mkdir -p "$wt_root/git-home"
  export HOME="$wt_root/git-home" XDG_CONFIG_HOME="$wt_root/git-home/.config"
}
wt_root="$SANDBOX/wt"
wt_home="$wt_root/home"
mkdir -p "$wt_home"
wt_ok=1
(
  set -e
  fixture_git_env
  mkdir -p "$wt_root/main"
  cd "$wt_root/main"
  git init -q --template= .
  git config user.email t@example.invalid
  git config user.name t
  git commit -q --allow-empty -m init
  mkdir -p .claude
  printf '%s' '{"mcpServers":{"coord-mcp":{}}}' > .mcp.json
  printf '%s' '{"enabledMcpjsonServers":["coord-mcp"]}' > .claude/settings.json
  # Options BEFORE the path. `git worktree add <path> -b probe` can be read with
  # `-b probe` in the <commit-ish> position; a failure there would take the SKIP
  # branch below and quietly retire this case's coverage, which is the outcome
  # the skip branch exists to make visible rather than to cause.
  git worktree add -q -b probe "$wt_root/linked"
) >/dev/null 2>&1
# The status is read on its own line rather than by `|| wt_ok=0` on the
# subshell: bash switches set -e off inside any command whose status is
# tested, so a failing step there ran on and the subshell reported only its
# last command. Read this way set -e is live, and the first failing step --
# a commit a signing setting refused, say -- is the status. A tested
# `if ( ... )` would switch it off the same way.
# shellcheck disable=SC2181
[ $? -eq 0 ] || wt_ok=0

if [ "$wt_ok" = "0" ] || [ ! -d "$wt_root/linked" ]; then
  # A git that cannot build a worktree here is a LOCAL limitation. Say so; do not
  # report it as a passing assertion, and do not fail the suite for it either --
  # a skip that announces itself is honest, a silent pass is not.
  echo "  SKIP worktree trust anchor: could not build a throwaway repo + worktree (local git limitation)"
else
  # The trust key must be spelled the way GIT spells it, not the way mktemp
  # does. Under Git Bash `mktemp -d` yields a POSIX path (/tmp/tmp.XXXX) while
  # `git rev-parse --path-format=absolute` yields a native one
  # (C:/Users/.../tmp.XXXX) -- so a fixture written with $wt_root/main would
  # never match the key the script looks up, and BOTH assertions below would
  # fail on Windows while passing on CI, looking exactly like a resolver bug.
  # Derive it through the same command the script uses. (Production is
  # unaffected: the real ~/.claude.json already stores forward-slash drive
  # paths, the same form git emits.)
  wt_key="$(fixture_git_env; cd "$wt_root/main" && dirname "$(git rev-parse --path-format=absolute --git-common-dir)")"
  # main checkout trusted; the worktree path deliberately is NOT.
  printf '{"projects":{"%s":{"hasTrustDialogAccepted":true}}}' "$wt_key" > "$wt_home/.claude.json"
  # The worktree needs the declaration too -- $PWD/.mcp.json is read from the cwd.
  printf '%s' '{"mcpServers":{"coord-mcp":{}}}' > "$wt_root/linked/.mcp.json"
  # This run gets $COORD_HTTP_URL from PINNED_ENV like every other. When each
  # invocation still listed its own pins, this one once left it out, which
  # contradicted the header's own belt-AND-braces rule -- the rule exists
  # precisely because the belt ($QONTINUI_RUNNER_URL) silently broke once.
  # Nothing reached production through that gap (the static credentials were
  # empty and the mint's origin a dead port, so L3/L4 had nothing to send), but
  # "currently sufficient" is the reasoning the header rejects, and one shared
  # list is what stops a new invocation repeating it.
  wt_out="$(cd "$wt_root/linked" && env -i "${PINNED_ENV[@]}" PWD="$PWD" \
            HOME="$wt_home" USERPROFILE="$wt_home" \
            CLAUDE_CONFIG_DIR="" QONTINUI_ROOT="$wt_root" \
            bash "$SCRIPT" 2>&1)"
  check_origins "linked worktree" "$wt_out"
  # $wt_key, NOT $wt_root/main. The comment above worked out that git and mktemp
  # spell this directory differently and then fixed only the FIXTURE; both
  # assertions kept the mktemp spelling and so failed on Windows against a script
  # that was answering correctly -- reporting a resolver bug that did not exist.
  if printf '%s' "$wt_out" | grep -q "APPROVAL: trust $wt_key: ACCEPTED"; then
    PASS=$((PASS + 1)); echo "  ok   a linked worktree reads the MAIN checkout's trust, not its own path"
  else
    FAIL=$((FAIL + 1)); FAILED_CASES+=("worktree trust anchor")
    echo "  FAIL a linked worktree did not resolve trust to the main checkout"
    echo "       expected: APPROVAL: trust $wt_key: ACCEPTED"
    printf '%s\n' "$wt_out" | grep "APPROVAL: trust" | sed 's/^/       /'
  fi
  # The settings layers use the OPPOSITE anchor, and that is the point of testing
  # them in the same fixture: trust keys on the MAIN checkout while project
  # layers are read at $PWD. Two anchors, deliberately.
  #
  # This assertion used to demand the reverse -- that project-shared be read at
  # the main checkout -- which is the behaviour coord-revive.sh's own comment
  # records as "this block's first bug (caught in pre-PR review)": reading the
  # approval at the main checkout while reading the declaration at $PWD tells a
  # session in `agent-worktrees/<uuid>` NO_APPROVAL_FOUND when its own
  # settings.local.json approves coord-mcp, which on this fleet is the DEFAULT
  # path rather than an edge. The assertion was never run, so it never got to
  # block the fix; had it run it would have argued for the bug.
  #
  # The main checkout's settings.json is uncommitted, so `git worktree add` never
  # checked it out and the worktree genuinely has none -- which makes `absent` at
  # the WORKTREE path the sharp reading here: a resolver that had fallen back to
  # the main checkout would report `enabledMcpjsonServers=named` instead.
  #
  # $wt_root SPELLING, not $wt_key's, and the difference is not an oversight:
  # this path comes from the subject's $PWD, which the run above passes on
  # after its `cd`, so it wears that cd's spelling -- the mktemp POSIX one, a
  # symlink in $TMPDIR included. (Under env -i alone the subject's bash would
  # spell it the physical way instead.) Only the TRUST key travels through git
  # and so wears git's native spelling. Two anchors, two spellings, from the
  # same fixture.
  if printf '%s' "$wt_out" | grep -q "APPROVAL: project-shared $wt_root/linked/.claude/settings.json: absent or unreadable"; then
    PASS=$((PASS + 1)); echo "  ok   the settings layers are read at \$PWD (the worktree), NOT at the trust anchor"
  else
    FAIL=$((FAIL + 1)); FAILED_CASES+=("worktree settings anchor")
    echo "  FAIL the project-shared layer was not read at \$PWD"
    echo "       expected: APPROVAL: project-shared $wt_root/linked/.claude/settings.json: absent or unreadable"
    printf '%s\n' "$wt_out" | grep "APPROVAL: project-shared" | sed 's/^/       /'
  fi
fi

fi

if mc_sec configdir; then
# --- 12. $CLAUDE_CONFIG_DIR RELOCATES BOTH USER-LEVEL FILES -------------------
# The fixture puts the ONLY approval, and the ONLY trust entry, inside a config
# dir that is not derived from $HOME -- and leaves $HOME empty. So the approval
# is reachable through the resolver and through nothing else.
#
# THE NEGATIVE CONTROL IS THE POINT. The same tree read with $CLAUDE_CONFIG_DIR
# unset must find nothing: that is what separates "the resolver works" from "some
# file somewhere was read". Without it a script that ignored the variable
# entirely -- which is exactly what shipped before this case -- would still pass
# the positive half on any box where $HOME happened to hold an approval.
echo "  -- CLAUDE_CONFIG_DIR resolution --"
cfg_root="$SANDBOX/cfgdir"
cfg_proj="$cfg_root/proj"
cfg_home="$cfg_root/home"
cfg_cfg="$cfg_root/cfg"
mkdir -p "$cfg_proj/.claude" "$cfg_home" "$cfg_cfg"
decl_coord "$cfg_proj"
# Both user-level files, at the two paths $CLAUDE_CONFIG_DIR puts them -- side by
# side, which is NOT the shape they have when it is unset (settings live one
# directory down, the store does not). Covering both in one run is deliberate:
# the resolver is per-file, so a prefix-swap that got only one right would pass
# a single-file assertion.
printf '%s' '{"enabledMcpjsonServers":["coord-mcp"]}' > "$cfg_cfg/settings.json"
printf '{"projects":{"%s":{"hasTrustDialogAccepted":true}}}' "$cfg_proj" > "$cfg_cfg/.claude.json"
cfg_run() {  # <claude-config-dir value>
  (cd "$cfg_proj" && env -i "${PINNED_ENV[@]}" PWD="$PWD" \
     HOME="$cfg_home" USERPROFILE="$cfg_home" \
     CLAUDE_CONFIG_DIR="$1" QONTINUI_ROOT="$cfg_proj" \
     bash "$SCRIPT" 2>&1)
}
cfg_on="$(cfg_run "$cfg_cfg")"
cfg_off="$(cfg_run "")"
check_origins "CLAUDE_CONFIG_DIR set" "$cfg_on"
check_origins "CLAUDE_CONFIG_DIR empty" "$cfg_off"
cfg_want_settings="APPROVAL: user $cfg_cfg/settings.json: enabledMcpjsonServers=named"
cfg_want_store="APPROVAL: user-store $cfg_cfg/.claude.json"
if printf '%s' "$cfg_on" | grep -q "$cfg_want_settings" \
   && printf '%s' "$cfg_on" | grep -q "$cfg_want_store" \
   && printf '%s' "$cfg_on" | grep -q "APPROVAL: APPROVED_UNGATED"; then
  PASS=$((PASS + 1)); echo "  ok   \$CLAUDE_CONFIG_DIR relocates both the user settings file and the user store"
else
  FAIL=$((FAIL + 1)); FAILED_CASES+=("CLAUDE_CONFIG_DIR resolution")
  echo "  FAIL \$CLAUDE_CONFIG_DIR was not honoured for both user-level files"
  echo "       expected: $cfg_want_settings"
  echo "       expected: $cfg_want_store"
  echo "       expected: APPROVAL: APPROVED_UNGATED"
  printf '%s\n' "$cfg_on" | grep -E "APPROVAL: (user|APPROVAL_|APPROVED_|NO_APPROVAL)" | sed 's/^/       /'
fi
# ...and unset, the very same tree must come up empty.
if printf '%s' "$cfg_off" | grep -q "APPROVAL: NO_APPROVAL_FOUND"; then
  PASS=$((PASS + 1)); echo "  ok   with \$CLAUDE_CONFIG_DIR unset the same fixture is NOT found (control)"
else
  FAIL=$((FAIL + 1)); FAILED_CASES+=("CLAUDE_CONFIG_DIR negative control")
  echo "  FAIL the config-dir fixture was reachable without \$CLAUDE_CONFIG_DIR - the positive case above proves nothing"
  printf '%s\n' "$cfg_off" | grep -E "APPROVAL: (user|APPROVAL_|APPROVED_|NO_APPROVAL)" | sed 's/^/       /'
fi

fi

if mc_sec roster; then
# --- 11. THE DOCUMENTED ROSTER IS THE ONE THE SCRIPT CAN EMIT -----------------
# The cases above prove each token is REACHABLE. This proves the set is
# DOCUMENTED -- a different property, and the one that had already broken.
#
# The summary table in SKILL.md is what an agent greps when it sees an APPROVAL:
# line it does not recognise. When this assertion was written the script could
# emit EIGHT tokens and the table listed SIX: `DECLARATION_UNKNOWN` and
# `APPROVAL_TRUST_UNKNOWN` were added to the summary during pre-PR review and
# never reached the table. `APPROVAL_TRUST_UNKNOWN` is the token a fleet session
# most often sees -- the tracked `.claude/settings.json` PR #370 added is a
# repository-supplied layer, and an agent worktree's main checkout frequently has
# no `projects` entry in ~/.claude.json -- so the most common reading was the one
# an agent could not look up. A partial list reads as a complete one.
#
# BOTH directions, because they fail differently: a token with no row is an
# undocumented output, and a row naming no token is a reader sent hunting for a
# string this script cannot produce.
#
# LOCATED BY MARKER, not by document shape. "The first table after the APPROVAL
# heading" silently re-points itself the first time someone adds a heading, and a
# roster check that reads the wrong table is worse than none -- it reports green
# about a list nobody maintains.
#
# EMPTINESS IS A FAILURE, never a pass. Both extractors are regexes over files
# they do not own; the way this check dies is that one of them stops matching and
# `comm` then cheerfully reports two empty sets as agreeing. That is the
# green-by-construction shape, so each side is asserted non-empty BEFORE the
# comparison, with its own message naming which extractor went blind.
#
# WHAT THE EMPTY-GUARD DOES NOT CATCH, stated because a guard's advertised reach
# is the thing people trust: it fires only on TOTAL blindness. The script-side
# pattern matches literal `APPROVAL_VERDICT="TOKEN` assignments, so an arm
# written as `APPROVAL_VERDICT="$foo"` leaves the emitted set silently while the
# other seven still match and `comm` still reports agreement. Every arm is
# literal today -- that is a convention this check assumes, not one it enforces.
echo "  -- summary roster agreement --"
SKILL_MD="$HERE/SKILL.md"
roster_fail() {
  FAIL=$((FAIL + 1)); FAILED_CASES+=("summary roster agreement"); echo "  FAIL $1"
}
if [ ! -r "$SKILL_MD" ]; then
  roster_fail "SKILL.md not readable at $SKILL_MD"
else
  # `APPROVAL_VERDICT="` with the quote pinned: the guard in
  # approval_verdict_block reads ${APPROVAL_VERDICT:-} and the final note passes
  # "$APPROVAL_VERDICT", neither of which is an assignment of a literal token.
  # Every stage in the C locale. Under tr_TR.UTF-8, GNU grep's [A-Z] does not
  # match I, so this check reported DECLARAT and NOT_APPL as undocumented
  # about a table that agrees with the script -- measured, and present at base.
  ROSTER_EMITTED="$(LC_ALL=C grep -oE 'APPROVAL_VERDICT="[A-Z_]+' "$SCRIPT" | LC_ALL=C sed 's/.*"//' | LC_ALL=C sort -u)"
  ROSTER_DOCUMENTED="$(LC_ALL=C sed -n '/APPROVAL-SUMMARY-ROSTER: begin/,/APPROVAL-SUMMARY-ROSTER: end/p' "$SKILL_MD" \
                       | LC_ALL=C grep -oE '^\| `[A-Z_]+`' | LC_ALL=C tr -d '|` ' | LC_ALL=C sort -u)"
  if [ -z "$ROSTER_EMITTED" ]; then
    roster_fail "no APPROVAL_VERDICT assignments matched in coord-revive.sh - the SCRIPT-side extractor went blind, so this check proves nothing"
  elif [ -z "$ROSTER_DOCUMENTED" ]; then
    roster_fail "no token rows found between the APPROVAL-SUMMARY-ROSTER markers in SKILL.md - the markers are missing, moved, or the table's row shape changed"
  else
    ROSTER_UNDOC="$(LC_ALL=C comm -23 <(printf '%s\n' "$ROSTER_EMITTED") <(printf '%s\n' "$ROSTER_DOCUMENTED"))"
    ROSTER_ORPHAN="$(LC_ALL=C comm -13 <(printf '%s\n' "$ROSTER_EMITTED") <(printf '%s\n' "$ROSTER_DOCUMENTED"))"
    if [ -z "$ROSTER_UNDOC" ] && [ -z "$ROSTER_ORPHAN" ]; then
      PASS=$((PASS + 1))
      echo "  ok   all $(printf '%s\n' "$ROSTER_EMITTED" | wc -l | tr -d ' ') summary tokens are documented, and no row names a token the script cannot emit"
    else
      roster_fail "the SKILL.md summary table and coord-revive.sh disagree"
      [ -z "$ROSTER_UNDOC" ] || { echo "       emitted but NOT documented:"; printf '%s\n' "$ROSTER_UNDOC" | sed 's/^/         /'; }
      [ -z "$ROSTER_ORPHAN" ] || { echo "       documented but NOT emitted:"; printf '%s\n' "$ROSTER_ORPHAN" | sed 's/^/         /'; }
    fi
  fi
fi

fi

# --- the discharge: the origin sentinel is shown able to fail --------------
# Only a whole, unmutated run drives it (mc_graded): a re-run never recurses,
# and a narrowed run has not run the cases a trailer would describe.
if mc_graded; then
    echo "  -- discharge --"
    # Staged under $SANDBOX/stage, so the mutant is
    # $SANDBOX/stage/mutation-control/m1/coord-revive.sh: three directories
    # below $SANDBOX, where its resolver's first two rungs find $SANDBOX/scripts.
    mc_init ".claude/skills/coord-revive/approval-half-test.sh" "$SANDBOX/stage"
    # Both re-runs are narrowed to the configdir section: two subject runs,
    # both read by the sentinel, and no git fixture that could SKIP. The
    # staging control -- an unmutated copy in the mutant's own layout, which
    # must run green there, and its rung-1 check -- lives in the shared helper.
    isolation_staging_control "$SCRIPT" "$REPO_ROOT" -- bash "$0" --only configdir
    # The mutation floor-claim-test.sh drives too. L6 is re-pointed at ANOTHER
    # loopback address rather than back at its remote default, so the re-run
    # that must redden sends nothing off the box either.
    mc_expect_red "L6 ignores its override, so the web host is not the pinned dead port" \
      "$SCRIPT" 's|^WEB_URL="\${QONTINUI_WEB_HTTP_URL:-https://api.qontinui.io}"$|WEB_URL="http://127.0.0.2:1"|' \
      -- bash "$0" --only configdir
    mc_trailer "$((PASS + FAIL))"
fi

echo
if [ "$FAIL" -eq 0 ]; then
  echo "coord-revive APPROVAL half: all $PASS assertions passed."
  exit 0
fi
echo "coord-revive APPROVAL half: $FAIL of $((PASS + FAIL)) assertions FAILED:"
for c in "${FAILED_CASES[@]}"; do echo "  $c"; done
exit 1
