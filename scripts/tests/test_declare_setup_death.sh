#!/usr/bin/env bash
# Fixture tests for .github/scripts/declare-setup-death.sh and its wiring into
# clippy-tiers.yml. Builds a real origin repo, a PR branch and a no-ff merge
# commit (the shape actions/checkout gives a pull_request run), clones it at
# depth 1 (so the merge is grafted and the base must be fetched, as in CI),
# then asserts declare vs withhold. No network.
# Run: bash scripts/tests/test_declare_setup_death.sh
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
WORKFLOW="$ROOT/.github/workflows/clippy-tiers.yml"
JOB='Clippy diff-scoped (advisory)'
DECLARE="::error title=${JOB} SETUP FAILED (not a verdict on this PR)::"
PHRASE='(not a verdict on this PR)'
HARNESS='^(\.github/(workflows/clippy-tiers\.yml|actions/|sibling-pins\.conf)|rust-toolchain(\.toml)?$)'

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1

pass=0
fail=0
ok() { pass=$((pass + 1)); echo "ok   - $1"; }
bad() { fail=$((fail + 1)); echo "FAIL - $1"; [ -n "${2:-}" ] && printf '       %s\n' "$2"; }

# make_pr <name> <path>... : echoes "<workspace> <merge sha>".
make_pr() {
  local name="$1"; shift
  local o="$TMP/$name-origin" w="$TMP/$name-work" p
  git init -q -b main "$o"
  git -C "$o" config uploadpack.allowAnySHA1InWant true
  mkdir -p "$o/src-tauri" && echo base > "$o/src-tauri/lib.rs" && echo r > "$o/README.md"
  git -C "$o" add -A && git -C "$o" commit -q -m base
  git -C "$o" checkout -q -b pr
  for p in "$@"; do
    mkdir -p "$o/$(dirname "$p")"
    echo "change $RANDOM" >> "$o/$p"
  done
  git -C "$o" add -A && git -C "$o" commit -q -m pr
  git -C "$o" checkout -q main
  echo main2 >> "$o/README.md" && git -C "$o" commit -q -am main2
  git -C "$o" merge -q --no-ff -m merge pr
  git clone -q --depth 1 "file://$o" "$w" 2> /dev/null
  echo "$w $(git -C "$o" rev-parse HEAD)"
}

T=$'\t'
# The clippy-diff job's setup steps and the class each MUST carry. The wiring
# check below asserts clippy-tiers.yml carries exactly these classes, so the
# decision cases exercise the shipped classification, not a copy of it.
STEP_TABLE="$(cat << 'ROWS'
Checkout|tree-free
Check out sibling qontinui-schemas|sibling-labels
Checkout sibling repos|tree-free
Setup pnpm|frontend
Setup Node.js|frontend
Setup Rust|tree-free
Setup reviewdog|tree-free
Install dependencies (Ubuntu)|tree-free
Free disk space (Ubuntu)|tree-free
Add swapfile (Ubuntu — guard against link-time OOM)|tree-free
Cache Rust dependencies|tree-free
Install frontend dependencies|frontend
Build frontend|frontend
ROWS
)"
# steps <failed step name|-> : the setup rows, that one failed.
steps() {
  local f="$1" seen=0 out="" name class
  while IFS='|' read -r name class; do
    if [ "$name" = "$f" ]; then out+="$name${T}failure${T}$class"$'\n'; seen=1
    elif [ "$seen" = 1 ]; then out+="$name${T}skipped${T}$class"$'\n'
    else out+="$name${T}success${T}$class"$'\n'; fi
  done <<< "$STEP_TABLE"
  printf '%s' "$out"
}
VERDICT_SKIPPED="Clippy diff-scoped via reviewdog${T}skipped"

# reporter <env...> : the reporter under the GitHub runner's locale, with the
# case's env assignments applied last (so they override the defaults).
reporter() {
  local -a extra=("$@")
  env -i PATH="$PATH" HOME="$HOME" LANG=C.UTF-8 LC_ALL=C.UTF-8 GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
    DECLARE_JOB="$JOB" HARNESS_PATHS_RE="$HARNESS" VERDICT_STEPS="$VERDICT_SKIPPED" \
    GITHUB_EVENT_NAME=pull_request "${extra[@]}" bash "$ROOT/.github/scripts/declare-setup-death.sh" 2>&1
}

# run <label> <expect: declare|withhold|silent> [why=<substring>] <env...>
# A withhold names the reason it must carry, so a withhold for the WRONG
# reason (an unreadable diff where the harness guard should have fired) fails.
run() {
  local label="$1" expect="$2"; shift 2
  local why="" out rc got
  case "${1:-}" in why=*) why="${1#why=}"; shift ;; esac
  out="$(reporter "$@")"
  rc=$?
  if grep -qF "$DECLARE" <<< "$out"; then got=declare
  elif grep -qF '::notice title=' <<< "$out"; then got=withhold
  elif [ -z "$out" ]; then got=silent
  else got="other"
  fi
  if [ "$got" = withhold ] && grep -qF "$PHRASE" <<< "$out"; then got="withhold-leaks-phrase"; fi
  if [ -n "$why" ] && ! grep -qF "$why" <<< "$out"; then got="$got-without-reason[$why]"; fi
  if [ "$rc" -eq 0 ] && [ "$got" = "$expect" ]; then ok "$label"
  else bad "$label: expected $expect rc=0, got $got rc=$rc" "$out"; fi
}

read -r W_RUST M_RUST <<< "$(make_pr rust src-tauri/src/x.rs Cargo.lock docs/n.md)"
read -r W_LOCK M_LOCK <<< "$(make_pr lock pnpm-lock.yaml)"
read -r W_PKG M_PKG <<< "$(make_pr pkg package.json)"
read -r W_TS M_TS <<< "$(make_pr ts src/x.ts)"
read -r W_WF M_WF <<< "$(make_pr wf .github/workflows/clippy-tiers.yml)"
read -r W_SELF M_SELF <<< "$(make_pr self .github/scripts/declare-setup-death.sh)"
read -r W_TC M_TC <<< "$(make_pr tc rust-toolchain.toml)"
read -r W_VENDOR M_VENDOR <<< "$(make_pr vendor vendor/pkg/index.js src-tauri/a.rs)"
read -r W_NL M_NL <<< "$(make_pr nl "a.md"$'\n'"src-tauri/x.html")"
read -r W_LAT M_LAT <<< "$(make_pr lat "src/caf"$'\xe9'".ts")"

pr() { echo GITHUB_WORKSPACE="$1" GITHUB_SHA="$2"; }

# shellcheck disable=SC2046
{
run "rust-only PR, pnpm install died -> declare" declare $(pr "$W_RUST" "$M_RUST") SETUP_STEPS="$(steps 'Install frontend dependencies')"
run "rust-only PR, frontend build died -> declare" declare $(pr "$W_RUST" "$M_RUST") SETUP_STEPS="$(steps 'Build frontend')"
run "rust-only PR, Setup pnpm died -> declare" declare $(pr "$W_RUST" "$M_RUST") SETUP_STEPS="$(steps 'Setup pnpm')"
run "lockfile PR, pnpm install died -> withhold" withhold "why=changes 'pnpm-lock.yaml'" $(pr "$W_LOCK" "$M_LOCK") SETUP_STEPS="$(steps 'Install frontend dependencies')"
run "package.json PR, Setup pnpm died -> withhold" withhold "why=changes 'package.json'" $(pr "$W_PKG" "$M_PKG") SETUP_STEPS="$(steps 'Setup pnpm')"
run "lockfile PR, Setup Node.js died -> withhold" withhold "why=changes 'pnpm-lock.yaml'" $(pr "$W_LOCK" "$M_LOCK") SETUP_STEPS="$(steps 'Setup Node.js')"
run "TS PR, frontend build died -> withhold" withhold "why=changes 'src/x.ts'" $(pr "$W_TS" "$M_TS") SETUP_STEPS="$(steps 'Build frontend')"
run "unclassified path (vendor/) PR, build died -> withhold" withhold "why=changes 'vendor/pkg/index.js'" $(pr "$W_VENDOR" "$M_VENDOR") SETUP_STEPS="$(steps 'Build frontend')"
run "path containing a newline, build died -> withhold" withhold "why=containing a newline" $(pr "$W_NL" "$M_NL") SETUP_STEPS="$(steps 'Build frontend')"
run "non-UTF-8 frontend path under a UTF-8 locale, build died -> withhold" withhold "why=reads this PR's frontend tree" $(pr "$W_LAT" "$M_LAT") SETUP_STEPS="$(steps 'Build frontend')"
run "TS PR, apt died -> declare (apt reads nothing of the PR)" declare $(pr "$W_TS" "$M_TS") SETUP_STEPS="$(steps 'Install dependencies (Ubuntu)')"
run "rust-only PR, apt died -> declare" declare $(pr "$W_RUST" "$M_RUST") SETUP_STEPS="$(steps 'Install dependencies (Ubuntu)')"
run "workflow PR, apt died -> withhold (harness)" withhold "why=own harness" $(pr "$W_WF" "$M_WF") SETUP_STEPS="$(steps 'Install dependencies (Ubuntu)')"
run "reporter-script PR, apt died -> withhold (always harness)" withhold "why=own harness" $(pr "$W_SELF" "$M_SELF") SETUP_STEPS="$(steps 'Install dependencies (Ubuntu)')"
run "rust-toolchain.toml PR, Setup Rust died -> withhold" withhold "why=own harness" $(pr "$W_TC" "$M_TC") SETUP_STEPS="$(steps 'Setup Rust')"
run "sibling resolver died on a rust-only PR -> withhold (live labels unreadable)" withhold "why=live labels" $(pr "$W_RUST" "$M_RUST") SETUP_STEPS="$(steps 'Check out sibling qontinui-schemas')"
run "sibling resolver died on a push -> withhold" withhold "why=live labels" GITHUB_EVENT_NAME=push SETUP_STEPS="$(steps 'Check out sibling qontinui-schemas')"
run "unknown class on an inert diff -> withhold (UNKNOWN)" withhold "why=no usable conclusion or class" $(pr "$W_RUST" "$M_RUST") SETUP_STEPS="Checkout${T}success${T}tree-free"$'\n'"Mystery${T}failure${T}sibling-label"
run "row with no class -> withhold (UNKNOWN)" withhold "why=no usable conclusion or class" $(pr "$W_RUST" "$M_RUST") SETUP_STEPS="Checkout${T}success${T}tree-free"$'\n'"Mystery${T}failure"
run "VERDICT_STEPS empty -> withhold (UNKNOWN)" withhold "why=VERDICT_STEPS carries no rows" $(pr "$W_RUST" "$M_RUST") VERDICT_STEPS= SETUP_STEPS="$(steps 'Install dependencies (Ubuntu)')"
run "nothing failed but a row is unreadable -> notice" withhold "why=whether a setup step failed is UNKNOWN" $(pr "$W_RUST" "$M_RUST") SETUP_STEPS="$(steps - | sed "s/^Setup Rust${T}success/Setup Rust${T}/")"
run "clippy step failed -> silent" silent $(pr "$W_RUST" "$M_RUST") VERDICT_STEPS="Clippy diff-scoped via reviewdog${T}failure" SETUP_STEPS="$(steps 'Build frontend')"
run "no setup step failed -> silent" silent $(pr "$W_RUST" "$M_RUST") SETUP_STEPS="$(steps -)"
run "empty conclusion row -> withhold (UNKNOWN)" withhold "why=rendered no usable conclusion" $(pr "$W_RUST" "$M_RUST") SETUP_STEPS="$(steps 'Build frontend' | sed "s/^Setup Rust${T}success/Setup Rust${T}/")"
run "DECLARE_JOB with a reserved phrase -> withhold" withhold "why=reserves" $(pr "$W_RUST" "$M_RUST") DECLARE_JOB="x TIMED OUT" SETUP_STEPS="$(steps 'Install dependencies (Ubuntu)')"
run "DECLARE_JOB carrying the excuse phrase, nothing failed -> withhold without leaking it" withhold "why=reserves" $(pr "$W_RUST" "$M_RUST") DECLARE_JOB="x (not a verdict on this PR)" SETUP_STEPS="$(steps -)"
run "DECLARE_JOB unset -> withhold" withhold "why=unset" $(pr "$W_RUST" "$M_RUST") DECLARE_JOB= SETUP_STEPS="$(steps 'Install dependencies (Ubuntu)')"
run "invalid HARNESS_PATHS_RE -> withhold (UNKNOWN)" withhold "why=not a valid ERE" $(pr "$W_TS" "$M_TS") HARNESS_PATHS_RE='^(oops' SETUP_STEPS="$(steps 'Build frontend')"
run "invalid FRONTEND_INERT_RE -> withhold (UNKNOWN)" withhold "why=not a valid ERE" $(pr "$W_TS" "$M_TS") FRONTEND_INERT_RE='[unclosed' SETUP_STEPS="$(steps 'Build frontend')"
run "garbled setup conclusion -> withhold (UNKNOWN)" withhold "why=rendered no usable conclusion" $(pr "$W_RUST" "$M_RUST") SETUP_STEPS="$(steps 'Build frontend' | sed "s/^Setup Rust${T}success/Setup Rust${T}sucess/")"
run "garbled verdict conclusion -> withhold (UNKNOWN)" withhold "why=rendered no usable conclusion" $(pr "$W_RUST" "$M_RUST") VERDICT_STEPS="Clippy diff-scoped via reviewdog${T}" SETUP_STEPS="$(steps 'Build frontend')"
run "no setup rows -> withhold (UNKNOWN)" withhold "why=no rows" $(pr "$W_RUST" "$M_RUST") SETUP_STEPS=
run "HARNESS_PATHS_RE unset -> withhold" withhold "why=HARNESS_PATHS_RE is unset" $(pr "$W_WF" "$M_WF") HARNESS_PATHS_RE= SETUP_STEPS="$(steps 'Install dependencies (Ubuntu)')"
run "non-numeric GIT_BOUND -> withhold" withhold "why=not a positive integer" $(pr "$W_RUST" "$M_RUST") GIT_BOUND=abc SETUP_STEPS="$(steps 'Install dependencies (Ubuntu)')"
run "step name with workflow-command metacharacters is escaped" declare "why=step 'a:b,c%25d' failed" $(pr "$W_RUST" "$M_RUST") SETUP_STEPS="Checkout${T}success${T}tree-free"$'\n'"a:b,c%d${T}failure${T}tree-free"
}
run "push event -> withhold (UNKNOWN)" withhold "why=not pull_request" GITHUB_EVENT_NAME=push GITHUB_WORKSPACE="$W_RUST" GITHUB_SHA="$M_RUST" SETUP_STEPS="$(steps 'Install frontend dependencies')"
run "schedule event -> withhold (UNKNOWN)" withhold "why=not pull_request" GITHUB_EVENT_NAME=schedule GITHUB_WORKSPACE="$W_RUST" GITHUB_SHA="$M_RUST" SETUP_STEPS="$(steps 'Install dependencies (Ubuntu)')"
NONMERGE="$(git -C "$TMP/rust-origin" rev-parse pr)"
git -C "$W_RUST" fetch -q --depth=1 origin "$NONMERGE" 2> /dev/null
run "non-merge GITHUB_SHA -> withhold (UNKNOWN)" withhold "why=not a PR merge commit" GITHUB_WORKSPACE="$W_RUST" GITHUB_SHA="$NONMERGE" SETUP_STEPS="$(steps 'Install frontend dependencies')"
run "unreadable GITHUB_SHA -> withhold (UNKNOWN)" withhold "why=not readable" GITHUB_WORKSPACE="$W_RUST" GITHUB_SHA=0123456789abcdef0123456789abcdef01234567 SETUP_STEPS="$(steps 'Build frontend')"
# A base the fetch cannot reach: point origin somewhere empty.
read -r W_GONE M_GONE <<< "$(make_pr gone src-tauri/y.rs)"
git -C "$W_GONE" remote set-url origin "file://$TMP/does-not-exist"
run "base unfetchable -> withhold (UNKNOWN)" withhold "why=could not be fetched" GITHUB_WORKSPACE="$W_GONE" GITHUB_SHA="$M_GONE" SETUP_STEPS="$(steps 'Install dependencies (Ubuntu)')"
# An empty PR diff: the merge commit's tree equals its first parent's.
EO="$TMP/empty-origin"; git init -q -b main "$EO"; echo a > "$EO/a"; git -C "$EO" add -A; git -C "$EO" commit -q -m base
git -C "$EO" checkout -q -b pr; git -C "$EO" commit -q --allow-empty -m empty; git -C "$EO" checkout -q main
git -C "$EO" merge -q --no-ff -m merge pr; EW="$TMP/empty-work"; git clone -q --depth 1 "file://$EO" "$EW" 2> /dev/null
run "empty PR diff -> withhold (UNKNOWN)" withhold "why=diff against its base is empty" GITHUB_WORKSPACE="$EW" GITHUB_SHA="$(git -C "$EO" rev-parse HEAD)" SETUP_STEPS="$(steps 'Install dependencies (Ubuntu)')"
# grep failing at run time (after the pattern probe, which reads /dev/null):
# its empty output must not read as "no match".
SHIM="$TMP/grep-shim"; mkdir -p "$SHIM"; REALGREP="$(command -v grep)"
# shellcheck disable=SC2016 # the shim's expansions run in the shim, not here
printf '#!/usr/bin/env bash\nin="$(cat)"\n[ -z "$in" ] && exec "%s" "$@" < /dev/null\nexit 2\n' "$REALGREP" > "$SHIM/grep"; chmod +x "$SHIM/grep"
# shellcheck disable=SC2046
run "grep errors over the diff -> withhold (UNKNOWN)" withhold "why=grep over this PR's diff failed" $(pr "$W_RUST" "$M_RUST") PATH="$SHIM:$PATH" SETUP_STEPS="$(steps 'Install dependencies (Ubuntu)')"
# Title-property escaping: ':' and ',' in the job name must be escaped.
# shellcheck disable=SC2046
out="$(reporter $(pr "$W_RUST" "$M_RUST") DECLARE_JOB='a: b, c' SETUP_STEPS="$(steps 'Install dependencies (Ubuntu)')")"
if grep -qF '::error title=a%3A b%2C c SETUP FAILED (not a verdict on this PR)::' <<< "$out"; then ok "title properties escape ':' and ','"
else bad "title properties escape ':' and ','" "$out"; fi
# A rename of a frontend input into an inert directory: --no-renames must
# list the source path, or only the inert destination is seen.
RO="$TMP/ren-origin"; git init -q -b main "$RO"; git -C "$RO" config uploadpack.allowAnySHA1InWant true
mkdir -p "$RO/src" && printf 'export const x = 1;\n%.0s' $(seq 40) > "$RO/src/x.ts"; git -C "$RO" add -A; git -C "$RO" commit -q -m base
git -C "$RO" checkout -q -b pr; mkdir -p "$RO/docs"; git -C "$RO" mv src/x.ts docs/x.ts; git -C "$RO" commit -q -m mv; git -C "$RO" checkout -q main
echo r > "$RO/README.md"; git -C "$RO" add -A; git -C "$RO" commit -q -m main2
git -C "$RO" merge -q --no-ff -m merge pr; RW="$TMP/ren-work"; git clone -q --depth 1 "file://$RO" "$RW" 2> /dev/null
run "frontend file renamed into docs/, build died -> withhold" withhold "why=changes 'src/x.ts'" GITHUB_WORKSPACE="$RW" GITHUB_SHA="$(git -C "$RO" rev-parse HEAD)" SETUP_STEPS="$(steps 'Build frontend')"

# --- wiring: clippy-tiers.yml's declare step must match its setup steps -----
if wiring="$(python3 - "$WORKFLOW" "$STEP_TABLE" << 'PY'
import re, sys
try:
    import yaml
except ImportError:
    print("PyYAML missing: the wiring check could not run"); sys.exit(1)
wf = yaml.safe_load(open(sys.argv[1]))
expected = dict(l.split("|", 1) for l in sys.argv[2].splitlines() if l)
steps = wf["jobs"]["clippy-diff"]["steps"]
errs = []
idx = next((i for i, s in enumerate(steps) if s.get("id") == "clippy"), None)
if idx is None:
    errs.append("no step with id: clippy")
    idx = 0
declare = steps[-1]
if declare.get("name") != "Declare a setup death (not a verdict on this PR)":
    errs.append("the declare step is not the job's last step")
env = declare.get("env", {})
run = declare.get("run", "")
by_id = {}
for k, v in env.items():
    m = re.fullmatch(r"\$\{\{ steps\.([a-z_]+)\.conclusion \}\}", str(v))
    if m:
        by_id[m.group(1)] = k
for s in steps[:idx]:
    sid, name = s.get("id"), s.get("name")
    if not sid:
        errs.append(f"setup step {name!r} has no id")
        continue
    var = by_id.get(sid)
    if not var:
        errs.append(f"setup step {name!r} has no steps.{sid}.conclusion env entry")
        continue
    row = re.search(r"row '" + re.escape(name) + r"' \"\$" + var + r"\" (tree-free|frontend|sibling-labels)\n", run)
    if not row:
        errs.append(f"setup step {name!r} has no `row` line carrying ${var} and a class")
    elif expected.get(name) != row.group(1):
        errs.append(f"setup step {name!r} is classed {row.group(1)!r}, the tested table says {expected.get(name)!r}")
names = [s.get("name") for s in steps[:idx]]
if names != list(expected):
    errs.append(f"setup steps {names!r} differ from the tested table {list(expected)!r}")
if "clippy" not in by_id:
    errs.append("the verdict step's conclusion is not passed")
elif not re.search(r'VERDICT_STEPS=.*"\$' + by_id["clippy"] + r'"', run):
    errs.append("VERDICT_STEPS is not built from the clippy step's conclusion")
title = "::error title=Clippy diff-scoped (advisory) SETUP FAILED (not a verdict on this PR)::"
if not re.search(r'if \[ "\$O_CHECKOUT" = "failure" \]; then\n\s+echo "' + re.escape(title), run):
    errs.append("the inline Checkout declaration is missing or its title drifted")
if run.count("(not a verdict on this PR)") != 1:
    errs.append("the reserved phrase appears in the declare step outside the single Checkout branch")
if env.get("DECLARE_JOB") != "Clippy diff-scoped (advisory)":
    errs.append("DECLARE_JOB drifted from the job's display name")
if declare.get("timeout-minutes") is None:
    errs.append("the declare step has no timeout-minutes")
print("\n".join(errs))
sys.exit(1 if errs else 0)
PY
)"; then ok "clippy-tiers.yml wiring: every pre-clippy step has an id, a conclusion env entry and the tested class"
else bad "clippy-tiers.yml wiring" "$wiring"; fi

# The workflow's harness RE must be the one these cases exercised.
if grep -qF "HARNESS_PATHS_RE: '$HARNESS'" "$WORKFLOW"; then ok "clippy-tiers.yml HARNESS_PATHS_RE matches the tested pattern"
else bad "clippy-tiers.yml HARNESS_PATHS_RE drifted from the tested pattern"; fi

echo "passed=$pass failed=$fail"
[ "$fail" -eq 0 ]
