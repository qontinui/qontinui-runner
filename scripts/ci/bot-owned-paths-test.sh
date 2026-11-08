#!/usr/bin/env bash
# shellcheck disable=SC2001,SC2016  # sed indents captured output; mutation seds are literal
# Fixture tests for scripts/ci/bot-owned-paths.sh — no network.
#
# `check` cases: (author, labels, label history, changed files) -> expected exit
# code, plus a string the output must contain, so a pass for the WRONG arm still
# reds. `pr` cases run the real `pr` subcommand against a STUBBED `gh` that holds
# JSON fixtures and applies the script's own --jq filter with a real jq, so the
# filters the script ships are exercised, not assumed.
#
# The last section re-runs the suite against mutants of the script and asserts
# each is caught by the case that guards it:
#   - arm (b), "the diff touches ONLY bot-owned paths (+ Cargo.lock)", removed
#     -> the pins-only case reds (the mutation Phase 0 names);
#   - arm (c)'s label-history read removed -> the stripped-label case reds;
#   - the truncation UNKNOWN turned into a pass -> the truncation case reds.
#
# Plan: qontinui-dev-notes plans/2026-11-04-runner-generated-and-ledger-files-are-
# merge-neutral-and-land-behind-a-drain.md, Phase 0.
#
# Run locally (needs jq): bash scripts/ci/bot-owned-paths-test.sh
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="${BOP_SCRIPT:-$here/bot-owned-paths.sh}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

pass=0 fail=0
n=0

report() {
  local name="$1" rc="$2" want="$3" needle="$4" out="$5"
  if [ "$rc" = "$want" ] && grep -qF -- "$needle" <<<"$out"; then
    pass=$((pass + 1))
    echo "ok   - $name"
  else
    fail=$((fail + 1))
    echo "FAIL - $name (rc=$rc want=$want, needle '$needle')"
    sed 's/^/       | /' <<<"$out"
  fi
}

# run_case NAME EXPECT_RC MUST_CONTAIN AUTHOR LABELS FILES [EVENTS]
#   LABELS, FILES and EVENTS are newline-separated strings; EVENTS lines are
#   `<event>\t<label>\t<actor>`.
run_case() {
  local name="$1" want="$2" needle="$3" author="$4" labels="$5" files="$6" events="${7:-}"
  n=$((n + 1))
  printf '%s' "$labels" >"$work/labels.$n"
  printf '%s' "$files" >"$work/files.$n"
  printf '%s' "$events" >"$work/events.$n"
  local out rc=0
  # GITHUB_STEP_SUMMARY unset so the script never writes outside $work.
  out="$(env -u GITHUB_STEP_SUMMARY bash "$SCRIPT" check "$author" "$work/labels.$n" "$work/files.$n" "$work/events.$n" 2>&1)" || rc=$?
  report "$name" "$rc" "$want" "$needle" "$out"
}

PINS=".github/sibling-pins.conf"
LOCK="Cargo.lock"
FC="src-tauri/src/fleet_commands/vet-plan.md"
FS="src-tauri/src/fleet_skills/coord-pr-label/SKILL.md"
FA="src-tauri/src/fleet_agents/repo-auditor.md"
CODE="src-tauri/src/mcp_api.rs"
T=$'\t'
S2="coord:downstream-of=qontinui-schemas#812"

run_case "feature PR touching sibling-pins.conf -> fail" 1 "FAIL:" \
  "jspinak" "" "$CODE"$'\n'"$PINS"
run_case "feature PR hand-copying a fleet command -> fail" 1 "FAIL:" \
  "jspinak" "coord:priority=high" "$FC"$'\n'"src/App.tsx"
run_case "feature PR hand-copying a fleet skill -> fail" 1 "FAIL:" \
  "jspinak" "" "$FS"$'\n'"$CODE"
run_case "feature PR hand-copying a fleet agent -> fail" 1 "FAIL:" \
  "jspinak" "" "$FA"$'\n'"$CODE"
run_case "pins-only PR -> pass (arm b)" 0 "arm (b)" \
  "jspinak" "" "$PINS"
run_case "pins + Cargo.lock PR -> pass (arm b)" 0 "arm (b)" \
  "jspinak" "" "$PINS"$'\n'"$LOCK"
run_case "hand bundle resync, commands + skills only -> pass (arm b)" 0 "arm (b)" \
  "jspinak" "" "$FC"$'\n'"$FS"
run_case "fleet_commands/x.md edit by the bundle bot -> pass (arm a)" 0 "arm (a): authored by qontinui-dev[bot]" \
  "qontinui-dev[bot]" "" "$FC"$'\n'"$CODE"
run_case "github-actions[bot] author -> pass (arm a)" 0 "arm (a): authored by github-actions[bot]" \
  "github-actions[bot]" "" "$PINS"$'\n'"$LOCK"$'\n'"$CODE"
run_case "schemas-pair adaptation, code + pin -> pass (arm c), applier named" 0 "AUDIT: arm (c) label $S2 (present; applied by jspinak)" \
  "jspinak" "$S2" "$CODE"$'\n'"$PINS"$'\n'"$LOCK" "labeled${T}${S2}${T}jspinak"
run_case "schemas-pair adaptation, owner-qualified -> pass (arm c)" 0 "arm (c)" \
  "jspinak" "coord:downstream-of=qontinui/qontinui-schemas#812" "$CODE"$'\n'"$PINS"
run_case "pair-follow pushed the pin after the label was stripped -> pass (arm c)" 0 "arm (c): declared schemas-leads adaptation ($S2, since removed by automation)" \
  "jspinak" "coord:landed" "$CODE"$'\n'"$PINS"$'\n'"$LOCK" \
  "labeled${T}${S2}${T}jspinak"$'\n'"unlabeled${T}${S2}${T}qontinui-merge-orchestrator[bot]"
run_case "pair-follow's own GITHUB_TOKEN delete also keeps arm (c)" 0 "arm (c)" \
  "jspinak" "" "$CODE"$'\n'"$PINS" \
  "labeled${T}${S2}${T}jspinak"$'\n'"unlabeled${T}${S2}${T}github-actions[bot]"
run_case "a schemas label a PERSON withdrew is not a declaration -> fail" 1 "FAIL:" \
  "jspinak" "" "$CODE"$'\n'"$PINS" \
  "labeled${T}${S2}${T}jspinak"$'\n'"unlabeled${T}${S2}${T}jspinak"
run_case "re-added by automation, then withdrawn by a person -> fail" 1 "FAIL:" \
  "jspinak" "" "$CODE"$'\n'"$PINS" \
  "labeled${T}${S2}${T}jspinak"$'\n'"unlabeled${T}${S2}${T}qontinui-merge-orchestrator[bot]"$'\n'"labeled${T}${S2}${T}github-actions[bot]"$'\n'"unlabeled${T}${S2}${T}octo"
run_case "every applier is named (a restore does not hide the person)" 0 "applied by jspinak,github-actions[bot]" \
  "jspinak" "$S2" "$CODE"$'\n'"$PINS" \
  "labeled${T}${S2}${T}jspinak"$'\n'"unlabeled${T}${S2}${T}github-actions[bot]"$'\n'"labeled${T}${S2}${T}github-actions[bot]"
run_case "downstream-of a RUNNER PR is not arm (c) -> fail" 1 "FAIL:" \
  "jspinak" "coord:downstream-of=qontinui-runner#2531" "$CODE"$'\n'"$PINS" \
  "labeled${T}coord:downstream-of=qontinui-runner#2531${T}jspinak"
run_case "near-miss schemas label (suffix) is not arm (c) -> fail" 1 "FAIL:" \
  "jspinak" "coord:downstream-of=qontinui-schemas#812x" "$CODE"$'\n'"$PINS"
run_case "override label -> pass, label, applier and paths named (arm d)" 0 "AUDIT: label ci:bot-owned-path-override (applied by jspinak) overrides bot-owned-paths for: .github/sibling-pins.conf" \
  "jspinak" "ci:gate-change=declared"$'\n'"ci:bot-owned-path-override" "$CODE"$'\n'"$PINS" \
  "labeled${T}ci:bot-owned-path-override${T}jspinak"
run_case "override applier unknown -> still named as UNKNOWN" 0 "applied by UNKNOWN" \
  "jspinak" "ci:bot-owned-path-override" "$CODE"$'\n'"$FC"
run_case "a removed override label does not pass -> fail" 1 "FAIL:" \
  "jspinak" "" "$CODE"$'\n'"$PINS" \
  "labeled${T}ci:bot-owned-path-override${T}jspinak"$'\n'"unlabeled${T}ci:bot-owned-path-override${T}jspinak"
run_case "look-alike bot login is not arm (a) -> fail" 1 "FAIL:" \
  "qontinui-dev" "" "$CODE"$'\n'"$PINS"
run_case "no bot-owned path -> pass" 0 "no bot-owned path touched" \
  "jspinak" "" "$CODE"$'\n'"$LOCK"
run_case "a nested Cargo.lock is not arm (b) scope -> fail" 1 "FAIL:" \
  "jspinak" "" "$PINS"$'\n'"vendor/tao-0.35.0/Cargo.lock"
run_case "fleet_commands.rs (the module, not the bundle) -> no bot path" 0 "no bot-owned path touched" \
  "jspinak" "" "src-tauri/src/fleet_commands.rs"$'\n'"src-tauri/src/fleet_commands_index.rs"
run_case "a % in a file name is escaped (%25) in the ::error command" 1 "::error title=bot-owned-paths::a bot-owned path (src-tauri/src/fleet_commands/a%250A::error::injected.md)" \
  "jspinak" "" "$CODE"$'\n'"src-tauri/src/fleet_commands/a%0A::error::injected.md"

# --- `pr` against a stubbed gh -----------------------------------------------
# The stub maps `gh api [--paginate] <path> [--jq <filter>]` to the fixture file
# $GH_FIX/<path with non-alphanumerics as _>; a sibling `.fail` file makes the
# call exit 1. Every fixture is the raw API JSON. A list endpoint read without
# --paginate fails, so dropping pagination reds the pr cases.
mkdir -p "$work/bin"
cat >"$work/bin/gh" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[ "${1:-}" = "api" ] || { echo "stub gh: only 'api' is stubbed" >&2; exit 2; }
shift
path="" jqf="" paged=0
while [ $# -gt 0 ]; do
  case "$1" in
    --paginate) paged=1; shift ;;
    --jq) jqf="$2"; shift 2 ;;
    *) path="$1"; shift ;;
  esac
done
f="$GH_FIX/$(printf '%s' "$path" | sed 's#[^A-Za-z0-9]#_#g')"
[ ! -e "$f.fail" ] || { echo "stub gh: HTTP 502 for $path" >&2; exit 1; }
[ -f "$f" ] || { echo "stub gh: no fixture for $path" >&2; exit 1; }
# A list endpoint read without --paginate would silently stop at one page.
case "$path" in
  */labels*|*/events*|*/files*)
    [ "$paged" = 1 ] || { echo "stub gh: list endpoint $path read without --paginate" >&2; exit 1; } ;;
esac
if [ -n "$jqf" ]; then jq -r "$jqf" "$f"; else cat "$f"; fi
STUB
chmod +x "$work/bin/gh"

R="qontinui/qontinui-runner"
fxp() { printf '%s' "$1" | sed 's#[^A-Za-z0-9]#_#g'; }

# pr_fixture DIR NUMBER AUTHOR CHANGED_FILES FILES_JSON [LABELS_JSON] [EVENTS_JSON]
pr_fixture() {
  local d="$1" num="$2" author="$3" changed="$4" files="$5" labels="${6:-[]}" events="${7:-[]}"
  mkdir -p "$d"
  printf '{"user":{"login":"%s"},"changed_files":%s}' "$author" "$changed" >"$d/$(fxp "repos/$R/pulls/$num")"
  printf '%s' "$labels" >"$d/$(fxp "repos/$R/issues/$num/labels?per_page=100")"
  printf '%s' "$events" >"$d/$(fxp "repos/$R/issues/$num/events?per_page=100")"
  printf '%s' "$files" >"$d/$(fxp "repos/$R/pulls/$num/files?per_page=100")"
}

# run_pr NAME EXPECT_RC MUST_CONTAIN FIXTURE_DIR NUMBER
run_pr() {
  local name="$1" want="$2" needle="$3" d="$4" num="$5" out rc=0
  out="$(env -u GITHUB_STEP_SUMMARY PATH="$work/bin:$PATH" GH_FIX="$d" bash "$SCRIPT" pr "$R" "$num" 2>&1)" || rc=$?
  report "$name" "$rc" "$want" "$needle" "$out"
}

FX="$work/fx"
pr_fixture "$FX/ok" 10 jspinak 2 '[{"filename":"src-tauri/src/mcp_api.rs"},{"filename":".github/sibling-pins.conf"}]'
run_pr "pr: feature + pin via the API -> fail" 1 "FAIL:" "$FX/ok" 10

pr_fixture "$FX/rename" 11 jspinak 2 '[{"filename":"src-tauri/src/mcp_api.rs"},{"filename":"docs/pins.conf","previous_filename":".github/sibling-pins.conf"}]'
run_pr "pr: renaming a bot-owned file away counts as an edit -> fail" 1 ".github/sibling-pins.conf" "$FX/rename" 11

pr_fixture "$FX/trunc" 12 jspinak 3001 '[{"filename":".github/sibling-pins.conf"}]'
run_pr "pr: truncated file list -> UNKNOWN (3), never a pass" 3 "UNKNOWN: PR #12 declares 3001" "$FX/trunc" 12

pr_fixture "$FX/lfail" 13 jspinak 1 '[{"filename":".github/sibling-pins.conf"}]'
touch "$FX/lfail/$(fxp "repos/$R/issues/13/labels?per_page=100").fail"
run_pr "pr: labels read fails -> UNKNOWN (3)" 3 "UNKNOWN: could not read PR #13 labels" "$FX/lfail" 13

pr_fixture "$FX/efail" 14 jspinak 1 '[{"filename":".github/sibling-pins.conf"}]'
touch "$FX/efail/$(fxp "repos/$R/issues/14/events?per_page=100").fail"
run_pr "pr: label-history read fails -> UNKNOWN (3)" 3 "UNKNOWN: could not read PR #14 label history" "$FX/efail" 14

pr_fixture "$FX/strip" 15 jspinak 3 \
  '[{"filename":"src-tauri/src/mcp_api.rs"},{"filename":".github/sibling-pins.conf"},{"filename":"Cargo.lock"}]' \
  '[{"name":"coord:landed"}]' \
  '[{"event":"labeled","label":{"name":"coord:downstream-of=qontinui-schemas#812"},"actor":{"login":"jspinak"}},{"event":"commented"},{"event":"unlabeled","label":{"name":"coord:downstream-of=qontinui-schemas#812"},"actor":{"login":"qontinui-merge-orchestrator[bot]"}}]'
run_pr "pr: stripped schemas label read from the history -> pass (arm c)" 0 "applied by jspinak" "$FX/strip" 15

pr_fixture "$FX/nl" 17 jspinak 2 '[{"filename":"Cargo.lock\nCargo.lock"},{"filename":".github/sibling-pins.conf"}]'
# (The bad name is FIRST on purpose: a check that read only jq's last output would miss it.)
run_pr "pr: a file name with a newline -> UNKNOWN (3), never split into fake paths" 3 "name contains CR or LF" "$FX/nl" 17

pr_fixture "$FX/ovr" 16 jspinak 2 \
  '[{"filename":"src-tauri/src/mcp_api.rs"},{"filename":"src-tauri/src/fleet_skills/x/SKILL.md"}]' \
  '[{"name":"ci:bot-owned-path-override"}]' \
  '[{"event":"labeled","label":{"name":"ci:bot-owned-path-override"},"actor":{"login":"octo"}}]'
run_pr "pr: override label applier read from the history -> pass (arm d)" 0 "applied by octo" "$FX/ovr" 16

echo "bot-owned-paths: $pass passed, $fail failed (of $((pass + fail)))"
[ "$fail" = 0 ] || exit 1

# --- mutation proofs ------------------------------------------------------------
# Skipped inside a mutant run.
if [ -z "${BOP_MUTANT:-}" ]; then
  # mutate NAME SED_EXPR CATCHING_CASE
  mutate() {
    local name="$1" expr="$2" catcher="$3" mutant="$work/mutant.sh" mout mrc
    sed "$expr" "$SCRIPT" >"$mutant"
    if cmp -s "$SCRIPT" "$mutant"; then
      echo "FAIL - mutation '$name': its pattern no longer matches the script; update this test"
      exit 1
    fi
    mout="$(BOP_MUTANT=1 BOP_SCRIPT="$mutant" bash "$0" 2>&1)" && mrc=0 || mrc=$?
    if [ "$mrc" != 0 ] && grep -qF "FAIL - $catcher" <<<"$mout"; then
      echo "ok   - mutation: $name is caught by '$catcher'"
    else
      echo "FAIL - mutation: $name was NOT caught by '$catcher'"
      sed 's/^/       | /' <<<"$mout"
      exit 1
    fi
  }
  mutate "dropping arm (b)" \
    's/if \[ "\${#others\[@\]}" -eq 0 \]; then/if false; then/' \
    "pins-only PR -> pass (arm b)"
  mutate "dropping arm (c)'s label-history read" \
    's/if \[ -z "\$form2" \] && \[ -n "\$events_file" \]; then/if false; then/' \
    "pair-follow pushed the pin after the label was stripped -> pass (arm c)"
  mutate "passing a truncated file list" \
    '/declares \$declared changed files/{n;s/exit 3/exit 0/;}' \
    "pr: truncated file list -> UNKNOWN (3), never a pass"
fi
