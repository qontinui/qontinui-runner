#!/usr/bin/env bash
# Exercise the REAL run-block extracted from .github/workflows/ci-integrity.yml.
#
# The embedded python3 surface-differ is stubbed here so the SHELL decision
# logic is what is under test; surface() and digest() are unit-tested separately
# by test-surface.py, against the same live workflow. Splitting them keeps this
# free of Windows/Git-Bash process plumbing that does not exist on the ubuntu
# runner the guard actually runs on.
#
# Findings format (what the stub emits, and what the shell consumes):
#   <KIND>\t<path>\t<token>      KIND = REMOVED | CHANGED | !PARSE
set -uo pipefail

# Self-contained: the run-block is extracted from the LIVE workflow each time,
# so these scenarios can never drift from the guard they are testing.
HERE="$(mktemp -d)"
trap 'rm -rf "$HERE"' EXIT
REPO_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
WF="${REPO_ROOT}/.github/workflows/ci-integrity.yml"
BLOCK="$HERE/run-block.sh"
PASS=0; FAIL=0

# Portability: CI is ubuntu (python3, POSIX paths). A developer running this from
# Git Bash on Windows has `python`, and its Windows build cannot write a
# `/tmp/...` path — so convert both paths for the interpreter. Probe that python3
# actually RUNS: on Windows a `python3.exe` App Execution Alias sits on PATH and
# satisfies `command -v` while executing nothing.
PY=python3; "$PY" -c "" >/dev/null 2>&1 || PY=python
PYWF="$WF"; PYBLOCK="$BLOCK"
if command -v cygpath >/dev/null 2>&1; then
  PYWF="$(cygpath -w "$WF")"; PYBLOCK="$(cygpath -w "$BLOCK")"
fi

"$PY" - "$PYWF" "$PYBLOCK" <<'EXTRACT'
import sys, yaml, io
wf, out = sys.argv[1], sys.argv[2]
d = yaml.safe_load(io.open(wf, encoding="utf-8"))
io.open(out, "w", encoding="utf-8", newline=chr(10)).write(
    d["jobs"]["guard-gating-workflows"]["steps"][0]["run"])
EXTRACT

mkdir -p "$HERE/bin"

cat > "$HERE/bin/gh" <<'STUB'
#!/usr/bin/env bash
set -uo pipefail
if [ "${1:-}" = "pr" ] && [ "${2:-}" = "comment" ]; then
  echo "COMMENT_POSTED" >> "$FIXTURE/comment.log"; exit 0
fi
if [ "${1:-}" = "api" ]; then
  for a in "$@"; do
    if [ "$a" = ".changed_files" ]; then
      if [ -f "$FIXTURE/changed_files.txt" ]; then cat "$FIXTURE/changed_files.txt"
      else grep -c "" "$FIXTURE/files.txt" 2>/dev/null || echo 0; fi
      exit 0
    fi
  done
  for a in "$@"; do
    case "$a" in
      */pulls/*/files)   cat "$FIXTURE/files.txt"  2>/dev/null; exit 0 ;;
      */issues/*/labels) cat "$FIXTURE/labels.txt" 2>/dev/null; exit 0 ;;
      */compare/*)       echo "mergebasesha";                   exit 0 ;;
      */pulls/[0-9]*)    cat "$FIXTURE/body.txt"   2>/dev/null; exit 0 ;;
    esac
  done
fi
exit 0
STUB
chmod +x "$HERE/bin/gh"

# Stands in for the embedded python3 surface-differ: emits the canned findings
# for this scenario and drains stdin (the heredoc'd script).
cat > "$HERE/bin/python3" <<'STUB'
#!/usr/bin/env bash
cat > /dev/null 2>/dev/null || true
cat "$FIXTURE/findings.tsv" 2>/dev/null
exit 0
STUB
chmod +x "$HERE/bin/python3"

run_case () {
  local name="$1" want_rc="$2" want_comment="$3" want_text="${4:-}"
  rm -f "$FIXTURE/comment.log"
  export RUNNER_TEMP="$FIXTURE/tmp"; mkdir -p "$RUNNER_TEMP"
  export GITHUB_STEP_SUMMARY="$FIXTURE/summary.md"; : > "$GITHUB_STEP_SUMMARY"
  export REPO="qontinui/qontinui-runner" PR_NUMBER=1 GH_TOKEN=x
  export BASE_SHA=base HEAD_SHA=head
  local out rc ok=1 why=""
  out="$(PATH="$HERE/bin:$PATH" bash "$BLOCK" 2>&1)"; rc=$?
  [ "$rc" -eq "$want_rc" ] || { ok=0; why="rc=$rc want=$want_rc"; }
  if [ "$want_comment" = "yes" ]; then
    [ -f "$FIXTURE/comment.log" ] || { ok=0; why="$why; no notification posted"; }
  else
    [ -f "$FIXTURE/comment.log" ] && { ok=0; why="$why; unexpected notification"; }
  fi
  # Search stdout AND the notification: the pass path writes its detail into the
  # summary/comment body rather than the job log.
  out="${out}
$(cat "$GITHUB_STEP_SUMMARY" 2>/dev/null)"
  if [ -n "$want_text" ] && ! grep -qF -- "$want_text" <<< "$out"; then
    ok=0; why="$why; missing text '$want_text'"
  fi
  if [ "$ok" -eq 1 ]; then
    echo "  PASS  $name"; PASS=$((PASS+1))
  else
    echo "  FAIL  $name -- $why"; FAIL=$((FAIL+1))
    printf '%s\n' "$out" | sed 's/^/        | /' | head -16
  fi
}

new_fixture () {
  FIXTURE="$HERE/fx"; rm -rf "$FIXTURE"; mkdir -p "$FIXTURE"
  export FIXTURE
  : > "$FIXTURE/labels.txt"; : > "$FIXTURE/body.txt"; : > "$FIXTURE/findings.tsv"
}

TAB="$(printf '\t')"
finding () { printf '%s%s%s%s%s\n' "$1" "$TAB" "$2" "$TAB" "$3" >> "$FIXTURE/findings.tsv"; }

echo "ci-integrity guard — shell decision logic"
echo ""

# --- scope -------------------------------------------------------------------
new_fixture
printf 'README.md\nsrc/main.rs\n' > "$FIXTURE/files.txt"
run_case "A no .github change -> green, no notification" 0 no "No gating workflow modified"

# The scope is now the TRIGGER, not a hand-maintained allowlist. A workflow that
# was never on the old list is guarded from the moment it exists — the allowlist
# failed open for exactly these, which is how secret-scan.yml went unguarded.
new_fixture
printf '.github/workflows/stacked-pr-fastlane.yml\n' > "$FIXTURE/files.txt"
run_case "B a workflow off the OLD allowlist is now guarded" 1 no "without declaring it"

new_fixture
printf '.github/actions/some-new-action/action.yml\n' > "$FIXTURE/files.txt"
run_case "C a brand-new composite action is guarded" 1 no "without declaring it"

# --- additive path -----------------------------------------------------------
new_fixture
printf '.github/workflows/ci.yml\n' > "$FIXTURE/files.txt"
run_case "D additive change, undeclared -> RED" 1 no "without declaring it"

printf 'ci:gate-change=declared\n' > "$FIXTURE/labels.txt"
run_case "E additive change, declared -> green + notification" 0 yes "purely additive"

# --- altered-surface path ----------------------------------------------------
finding CHANGED ".github/workflows/ci.yml" "ci#security"
run_case "F surface ALTERED, only the weak label -> RED" 1 no "alters an existing gate"

printf 'ci:gate-change=alters-a-gate\n' > "$FIXTURE/labels.txt"
printf 'A PR body that declares nothing.\n' > "$FIXTURE/body.txt"
run_case "G surface ALTERED, strong label but body silent -> RED" 1 no "Gate-Change: ci#security"

printf 'Reworked the security job.\n\nGate-Change: ci#security\n' > "$FIXTURE/body.txt"
run_case "H surface ALTERED, label + body names it -> green" 0 yes "ci#security"

# A REMOVAL needs the Gate-Removal directive; the Gate-Change line must not do.
new_fixture
printf '.github/workflows/ci.yml\n' > "$FIXTURE/files.txt"
finding REMOVED ".github/workflows/ci.yml" "ci#security"
printf 'ci:gate-change=alters-a-gate\n' > "$FIXTURE/labels.txt"
printf 'Gate-Change: ci#security\n' > "$FIXTURE/body.txt"
run_case "I a REMOVAL is not satisfied by a Gate-Change line" 1 no "Gate-Removal: ci#security"

printf 'Gate-Removal: ci#security\n' > "$FIXTURE/body.txt"
run_case "J a REMOVAL named with Gate-Removal -> green" 0 yes "REMOVED"

# Workflow-level triggers are part of the surface — this is the self-disable
# path (drop `.github/workflows/**` from ci-integrity's own `on.paths`).
new_fixture
printf '.github/workflows/ci-integrity.yml\n' > "$FIXTURE/files.txt"
finding CHANGED ".github/workflows/ci-integrity.yml" "ci-integrity#workflow"
printf 'ci:gate-change=declared\n' > "$FIXTURE/labels.txt"
run_case "K guard disabling ITSELF via on: is caught, not self-declarable" 1 no "alters an existing gate"

printf 'ci:gate-change=alters-a-gate\n' > "$FIXTURE/labels.txt"
printf 'Gate-Change: ci-integrity#workflow\n' > "$FIXTURE/body.txt"
run_case "L ...and passes only once explicitly named" 0 yes "ci-integrity#workflow"

# --- multiple findings -------------------------------------------------------
new_fixture
printf '.github/workflows/ci.yml\n' > "$FIXTURE/files.txt"
finding CHANGED ".github/workflows/ci.yml" "ci#security"
finding REMOVED ".github/workflows/ci.yml" "ci#seam-gate"
printf 'ci:gate-change=alters-a-gate\n' > "$FIXTURE/labels.txt"
printf 'Gate-Change: ci#security\n' > "$FIXTURE/body.txt"
run_case "M two findings, only one named -> RED" 1 no "Gate-Removal: ci#seam-gate"

printf 'Gate-Change: ci#security\nGate-Removal: ci#seam-gate\n' > "$FIXTURE/body.txt"
run_case "N two findings, both named -> green" 0 yes "ci#seam-gate"

# --- unparseable -------------------------------------------------------------
new_fixture
printf '.github/workflows/ci.yml\n' > "$FIXTURE/files.txt"
finding '!PARSE' ".github/workflows/ci.yml" "-"
printf 'ci:gate-change=alters-a-gate\n' > "$FIXTURE/labels.txt"
printf 'Gate-Change: anything\n' > "$FIXTURE/body.txt"
run_case "O unparseable gate -> RED even when fully declared" 1 no "Could not parse"

# --- rename ------------------------------------------------------------------
new_fixture
printf '.github/workflows/ci-old.yml\n.github/workflows/ci.yml\n' > "$FIXTURE/files.txt"
printf 'ci:gate-change=declared\n' > "$FIXTURE/labels.txt"
run_case "P rename reported via previous_filename is still a hit" 0 yes "ci.yml"

# ---------------------------------------------------------------------------
# Adversarial cases. Each of these made the guard GREEN in an earlier revision,
# so each is a regression test for a real bypass rather than a hypothetical.
# ---------------------------------------------------------------------------

# A body naming ONE item must not authorise a DIFFERENT one whose token it
# happens to contain. Unanchored `grep -F`, "ci#test-integration" also satisfied
# a job called "ci#test".
new_fixture
printf '.github/workflows/ci.yml\n' > "$FIXTURE/files.txt"
finding REMOVED ".github/workflows/ci.yml" "ci#test"
finding REMOVED ".github/workflows/ci.yml" "ci#test-integration"
printf 'ci:gate-change=alters-a-gate\n' > "$FIXTURE/labels.txt"
printf 'Gate-Removal: ci#test-integration\n' > "$FIXTURE/body.txt"
run_case "X1 prefix collision does not authorise the shorter token" 1 no "Gate-Removal: ci#test"

# The declaration has to be VISIBLE. Hiding it in an HTML comment defeats the
# durable-record property the whole design rests on.
new_fixture
printf '.github/workflows/ci.yml\n' > "$FIXTURE/files.txt"
finding REMOVED ".github/workflows/ci.yml" "ci#security"
printf 'ci:gate-change=alters-a-gate\n' > "$FIXTURE/labels.txt"
printf 'Nothing to see here.\n<!-- Gate-Removal: ci#security -->\n' > "$FIXTURE/body.txt"
run_case "X2 declaration hidden in an HTML comment does not count" 1 no "Gate-Removal: ci#security"

# Pasting the guard's own failure output must not satisfy it — the error text
# prints the required lines verbatim, indented.
printf 'CI said:\n  Gate-Removal: ci#security\n' > "$FIXTURE/body.txt"
run_case "X3 pasting the guard error output does not count" 1 no "Gate-Removal: ci#security"

# ...and the real thing still passes, so X1-X3 are not just "always red".
printf 'Gate-Removal: ci#security\n' > "$FIXTURE/body.txt"
run_case "X4 a plain, visible declaration line still passes" 0 yes "REMOVED"

# A large PR must not hide a gate edit. `printf | grep -q` returned 141 via
# SIGPIPE under `set -o pipefail` once CHANGED exceeded the pipe buffer, and the
# guard reported the repo intact. Measured: 500 files rc 0, 1500 files rc 141.
new_fixture
{ printf '.github/workflows/ci.yml\n'
  for i in $(seq 1 2000); do
    printf 'some/reasonably/long/vendored/path/number-%s/file-%s.generated.rs\n' "$i" "$i"
  done
} > "$FIXTURE/files.txt"
run_case "X5 a 2000-file PR cannot hide a gate edit" 1 no "without declaring it"

# A DECOY path must not erase a real gating file from the scan. The dedup test
# was an unanchored substring match, so `.github/actions/x/.github/workflows/
# ci.yml` (collected first, since `.github/actions/...` sorts earlier) swallowed
# the real `.github/workflows/ci.yml`, which was then never scanned at all --
# a total bypass reported as "purely additive".
new_fixture
printf '.github/actions/x/.github/workflows/ci.yml
.github/workflows/ci.yml
' > "$FIXTURE/files.txt"
printf 'ci:gate-change=declared
' > "$FIXTURE/labels.txt"
run_case "X6 a decoy path does not erase the real gating file" 0 yes "  - .github/workflows/ci.yml"

# GitHub caps /pulls/{n}/files at 3000 and truncates SILENTLY, which is the same
# "open a big PR" bypass in a different disguise. Scanning a prefix must be a
# hard error, not a pass.
new_fixture
printf '.github/workflows/ci.yml
' > "$FIXTURE/files.txt"
printf '4000
' > "$FIXTURE/changed_files.txt"
printf 'ci:gate-change=declared
' > "$FIXTURE/labels.txt"
run_case "X7 a truncated file list is a hard error, not a pass" 1 no "cannot certify anything"

# A gating file that uses YAML anchors at head cannot be certified: PyYAML
# resolves them and Actions does not support them, so an anchored rewrite
# digests as unchanged while the gate may stop running. Declaring it must not
# help -- there is nothing trustworthy to declare against.
new_fixture
printf '.github/workflows/ci.yml
' > "$FIXTURE/files.txt"
finding '!ALIAS' ".github/workflows/ci.yml" "-"
printf 'ci:gate-change=alters-a-gate
' > "$FIXTURE/labels.txt"
printf 'Gate-Change: anything
' > "$FIXTURE/body.txt"
run_case "X8 anchored gating file -> RED even when fully declared" 1 no "anchors, aliases or a merge key"

# An unreadable file count must FAIL, not skip the truncation guard. `set -e`
# does not fire for a failing command in an `if` CONDITION, so a non-numeric
# count made `[ "" -lt N ]` error and execution simply continued past the
# check -- disabling it exactly when the API is misbehaving.
new_fixture
printf '.github/workflows/ci.yml
' > "$FIXTURE/files.txt"
printf 'null
' > "$FIXTURE/changed_files.txt"
printf 'ci:gate-change=declared
' > "$FIXTURE/labels.txt"
run_case "X9 an unreadable file count is a hard error, not a skip" 1 no "certifies nothing"

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
