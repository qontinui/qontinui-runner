#!/usr/bin/env bash
# shellcheck disable=SC2001  # sed indents multi-line captured output
# Fixture tests for scripts/ci/bot-owned-paths.sh `check` — no network, no gh.
#
# Each case is (author, labels, changed files) -> expected exit code, plus a
# string the output must contain, so a pass for the WRONG arm still reds.
# The last section runs the suite against a mutant with arm (b) — "the diff
# touches ONLY bot-owned paths (+ Cargo.lock)" — removed, and asserts the
# pins-only case catches it.
#
# Plan: qontinui-dev-notes plans/2026-11-04-runner-generated-and-ledger-files-are-
# merge-neutral-and-land-behind-a-drain.md, Phase 0.
#
# Run locally: bash scripts/ci/bot-owned-paths-test.sh
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="${BOP_SCRIPT:-$here/bot-owned-paths.sh}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

pass=0 fail=0
n=0

# run_case NAME EXPECT_RC MUST_CONTAIN AUTHOR LABELS FILES [ACTOR]
#   LABELS and FILES are newline-separated strings.
run_case() {
  local name="$1" want="$2" needle="$3" author="$4" labels="$5" files="$6" actor="${7:-}"
  n=$((n + 1))
  printf '%s' "$labels" >"$work/labels.$n"
  printf '%s' "$files" >"$work/files.$n"
  local out rc=0
  # GITHUB_STEP_SUMMARY unset so the script never writes outside $work.
  out="$(env -u GITHUB_STEP_SUMMARY bash "$SCRIPT" check "$author" "$work/labels.$n" "$work/files.$n" ${actor:+"$actor"} 2>&1)" || rc=$?
  if [ "$rc" = "$want" ] && grep -qF -- "$needle" <<<"$out"; then
    pass=$((pass + 1))
    echo "ok   - $name"
  else
    fail=$((fail + 1))
    echo "FAIL - $name (rc=$rc want=$want, needle '$needle')"
    sed 's/^/       | /' <<<"$out"
  fi
}

PINS=".github/sibling-pins.conf"
LOCK="Cargo.lock"
FC="src-tauri/src/fleet_commands/vet-plan.md"
CODE="src-tauri/src/mcp_api.rs"

run_case "feature PR touching sibling-pins.conf -> fail" 1 "FAIL:" \
  "jspinak" "" "$CODE"$'\n'"$PINS"
run_case "feature PR hand-copying a fleet command -> fail" 1 "FAIL:" \
  "jspinak" "coord:priority=high" "$FC"$'\n'"src/App.tsx"
run_case "pins-only PR -> pass (arm b)" 0 "arm (b)" \
  "jspinak" "" "$PINS"
run_case "pins + Cargo.lock PR -> pass (arm b)" 0 "arm (b)" \
  "jspinak" "" "$PINS"$'\n'"$LOCK"
run_case "fleet_commands/x.md edit by the bundle bot -> pass (arm a)" 0 "arm (a): authored by qontinui-dev[bot]" \
  "qontinui-dev[bot]" "" "$FC"$'\n'"$CODE"
run_case "pin bump by github-actions -> pass (arm a)" 0 "arm (a): authored by github-actions[bot]" \
  "github-actions[bot]" "" "$PINS"$'\n'"$LOCK"$'\n'"$CODE"
run_case "schemas-pair adaptation, code + pin -> pass (arm c)" 0 "arm (c)" \
  "jspinak" "coord:downstream-of=qontinui-schemas#812" "$CODE"$'\n'"$PINS"$'\n'"$LOCK"
run_case "schemas-pair adaptation, owner-qualified -> pass (arm c)" 0 "arm (c)" \
  "jspinak" "coord:downstream-of=qontinui/qontinui-schemas#812" "$CODE"$'\n'"$PINS"
run_case "downstream-of a RUNNER PR is not arm (c) -> fail" 1 "FAIL:" \
  "jspinak" "coord:downstream-of=qontinui-runner#2531" "$CODE"$'\n'"$PINS"
run_case "near-miss schemas label (suffix) is not arm (c) -> fail" 1 "FAIL:" \
  "jspinak" "coord:downstream-of=qontinui-schemas#812x" "$CODE"$'\n'"$PINS"
run_case "override label -> pass, label named (arm d)" 0 "AUDIT: label ci:bot-owned-path-override (applied by jspinak)" \
  "jspinak" "ci:gate-change=declared"$'\n'"ci:bot-owned-path-override" "$CODE"$'\n'"$PINS" "jspinak"
run_case "override actor unknown -> still named as UNKNOWN" 0 "applied by UNKNOWN" \
  "jspinak" "ci:bot-owned-path-override" "$CODE"$'\n'"$FC"
run_case "look-alike bot login is not arm (a) -> fail" 1 "FAIL:" \
  "qontinui-dev" "" "$CODE"$'\n'"$PINS"
run_case "no bot-owned path -> pass" 0 "no bot-owned path touched" \
  "jspinak" "" "$CODE"$'\n'"$LOCK"
run_case "a nested Cargo.lock is not arm (b) scope -> fail" 1 "FAIL:" \
  "jspinak" "" "$PINS"$'\n'"vendor/tao-0.35.0/Cargo.lock"
run_case "fleet_commands directory name as a prefix only -> no bot path" 0 "no bot-owned path touched" \
  "jspinak" "" "src-tauri/src/fleet_commands_index.rs"

echo "bot-owned-paths: $pass passed, $fail failed (of $n)"
[ "$fail" = 0 ] || exit 1

# --- mutation proof ---------------------------------------------------------
# Drop arm (b) and assert the pins-only case reds. Skipped inside the mutant run.
if [ -z "${BOP_MUTANT:-}" ]; then
  mutant="$work/mutant.sh"
  # Arm (b) is the one `if` on an empty `others` array; neutralise its condition.
  sed 's/if \[ "\${#others\[@\]}" -eq 0 \]; then/if false; then/' "$SCRIPT" >"$mutant"
  if cmp -s "$SCRIPT" "$mutant"; then
    echo "FAIL - mutation: the arm (b) pattern no longer matches the script; update this test"
    exit 1
  fi
  mout="$(BOP_MUTANT=1 BOP_SCRIPT="$mutant" bash "$0" 2>&1)" && mrc=0 || mrc=$?
  if [ "$mrc" != 0 ] && grep -qF "FAIL - pins-only PR -> pass (arm b)" <<<"$mout"; then
    echo "ok   - mutation: dropping arm (b) is caught by the pins-only case"
  else
    echo "FAIL - mutation: dropping arm (b) was NOT caught by the pins-only case"
    sed 's/^/       | /' <<<"$mout"
    exit 1
  fi
fi
