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
SCRIPT="$ROOT/.github/scripts/declare-setup-death.sh"
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

# Event payloads for the sibling-labels class.
EV_NONE="$TMP/ev-none.json"; echo '{"pull_request":{"labels":[]}}' > "$EV_NONE"
EV_OTHER="$TMP/ev-other.json"; echo '{"pull_request":{"labels":[{"name":"bug"}]}}' > "$EV_OTHER"
EV_DEP="$TMP/ev-dep.json"; echo '{"pull_request":{"labels":[{"name":"coord:downstream-of=qontinui-schemas#12"}]}}' > "$EV_DEP"
EV_BAD="$TMP/ev-bad.json"; echo 'not json' > "$EV_BAD"

T=$'\t'
# steps <failed step name|-> : the clippy-diff job's setup rows, one failed.
steps() {
  local f="$1" seen=0 out="" name class
  while IFS='|' read -r name class; do
    if [ "$name" = "$f" ]; then out+="$name${T}failure${T}$class"$'\n'; seen=1
    elif [ "$seen" = 1 ]; then out+="$name${T}skipped${T}$class"$'\n'
    else out+="$name${T}success${T}$class"$'\n'; fi
  done << 'ROWS'
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
  printf '%s' "$out"
}
VERDICT_SKIPPED="Clippy diff-scoped via reviewdog${T}skipped"

# run <label> <expect: declare|withhold|silent> [why=<substring>] <env...>
# A withhold names the reason it must carry, so a withhold for the WRONG
# reason (an unreadable diff where the harness guard should have fired) fails.
run() {
  local label="$1" expect="$2"; shift 2
  local why="" out rc got
  case "${1:-}" in why=*) why="${1#why=}"; shift ;; esac
  out="$(env -i PATH="$PATH" HOME="$HOME" GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
    DECLARE_JOB="$JOB" HARNESS_PATHS_RE="$HARNESS" VERDICT_STEPS="$VERDICT_SKIPPED" \
    GITHUB_EVENT_NAME=pull_request GITHUB_EVENT_PATH="$EV_NONE" "$@" bash "$SCRIPT" 2>&1)"
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
run "TS PR, apt died -> declare (apt reads nothing of the PR)" declare $(pr "$W_TS" "$M_TS") SETUP_STEPS="$(steps 'Install dependencies (Ubuntu)')"
run "rust-only PR, apt died -> declare" declare $(pr "$W_RUST" "$M_RUST") SETUP_STEPS="$(steps 'Install dependencies (Ubuntu)')"
run "workflow PR, apt died -> withhold (harness)" withhold "why=own harness" $(pr "$W_WF" "$M_WF") SETUP_STEPS="$(steps 'Install dependencies (Ubuntu)')"
run "reporter-script PR, apt died -> withhold (always harness)" withhold "why=own harness" $(pr "$W_SELF" "$M_SELF") SETUP_STEPS="$(steps 'Install dependencies (Ubuntu)')"
run "rust-toolchain.toml PR, Setup Rust died -> withhold" withhold "why=own harness" $(pr "$W_TC" "$M_TC") SETUP_STEPS="$(steps 'Setup Rust')"
run "sibling step died, no labels -> declare" declare $(pr "$W_RUST" "$M_RUST") SETUP_STEPS="$(steps 'Check out sibling qontinui-schemas')"
run "sibling step died, unrelated label -> declare" declare $(pr "$W_RUST" "$M_RUST") GITHUB_EVENT_PATH="$EV_OTHER" SETUP_STEPS="$(steps 'Check out sibling qontinui-schemas')"
run "sibling step died, coord dep label -> withhold" withhold "why=carries 'coord:downstream-of" $(pr "$W_RUST" "$M_RUST") GITHUB_EVENT_PATH="$EV_DEP" SETUP_STEPS="$(steps 'Check out sibling qontinui-schemas')"
run "sibling step died, payload unparseable -> withhold (UNKNOWN)" withhold "why=could not be parsed" $(pr "$W_RUST" "$M_RUST") GITHUB_EVENT_PATH="$EV_BAD" SETUP_STEPS="$(steps 'Check out sibling qontinui-schemas')"
run "sibling step died, payload missing -> withhold (UNKNOWN)" withhold "why=payload is unreadable" $(pr "$W_RUST" "$M_RUST") GITHUB_EVENT_PATH="$TMP/nope.json" SETUP_STEPS="$(steps 'Check out sibling qontinui-schemas')"
run "unknown class -> treated as frontend" withhold "why=changes 'src/x.ts'" $(pr "$W_TS" "$M_TS") SETUP_STEPS="Checkout${T}success${T}tree-free"$'\n'"Mystery${T}failure${T}weird"
run "clippy step failed -> silent" silent $(pr "$W_RUST" "$M_RUST") VERDICT_STEPS="Clippy diff-scoped via reviewdog${T}failure" SETUP_STEPS="$(steps 'Build frontend')"
run "no setup step failed -> silent" silent $(pr "$W_RUST" "$M_RUST") SETUP_STEPS="$(steps -)"
run "empty conclusion row -> withhold (UNKNOWN)" withhold "why=rendered no conclusion" $(pr "$W_RUST" "$M_RUST") SETUP_STEPS="$(steps 'Build frontend' | sed "s/^Setup Rust${T}success/Setup Rust${T}/")"
run "DECLARE_JOB with a reserved phrase -> withhold" withhold "why=reserves" $(pr "$W_RUST" "$M_RUST") DECLARE_JOB="x TIMED OUT" SETUP_STEPS="$(steps 'Install dependencies (Ubuntu)')"
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

# --- wiring: clippy-tiers.yml's declare step must match its setup steps -----
if wiring="$(python3 - "$WORKFLOW" << 'PY'
import re, sys
try:
    import yaml
except ImportError:
    print("PyYAML missing: the wiring check could not run"); sys.exit(1)
wf = yaml.safe_load(open(sys.argv[1]))
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
if "clippy" not in by_id:
    errs.append("the verdict step's conclusion is not passed")
title = "::error title=Clippy diff-scoped (advisory) SETUP FAILED (not a verdict on this PR)::"
if not re.search(r'if \[ "\$O_CHECKOUT" = "failure" \]; then\n\s+echo "' + re.escape(title), run):
    errs.append("the inline Checkout declaration is missing or its title drifted")
if env.get("DECLARE_JOB") != "Clippy diff-scoped (advisory)":
    errs.append("DECLARE_JOB drifted from the job's display name")
if declare.get("timeout-minutes") is None:
    errs.append("the declare step has no timeout-minutes")
print("\n".join(errs))
sys.exit(1 if errs else 0)
PY
)"; then ok "clippy-tiers.yml wiring: every pre-clippy step has an id, a conclusion env entry and a classed row"
else bad "clippy-tiers.yml wiring" "$wiring"; fi

# The workflow's harness RE must be the one these cases exercised.
if grep -qF "HARNESS_PATHS_RE: '$HARNESS'" "$WORKFLOW"; then ok "clippy-tiers.yml HARNESS_PATHS_RE matches the tested pattern"
else bad "clippy-tiers.yml HARNESS_PATHS_RE drifted from the tested pattern"; fi

echo "passed=$pass failed=$fail"
[ "$fail" -eq 0 ]
