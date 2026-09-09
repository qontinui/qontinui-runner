#!/usr/bin/env bash
#
# Tests for `lib/ts-codegen-deps.sh` — the rung ladder that makes the
# TypeScript codegen's Node dependencies resolvable for `gen-events-drift.sh`.
#
# Run: bash .pre-commit-hooks/ts-codegen-deps-test.sh
#
# Every case builds a throwaway tree under $TMPDIR with FAKE npm packages, so
# nothing here needs a Rust toolchain, a real qontinui-schemas checkout, the
# ~minute release build the real hook pays, or (by default) a network. The one
# case that genuinely needs npm and a registry is opt-in:
#
#     QONTINUI_TS_CODEGEN_DEPS_TEST_NPM=1 bash .pre-commit-hooks/ts-codegen-deps-test.sh
#
# WHY THIS TEST EXISTS. The defect it pins was invisible to CI by construction:
# `.github/workflows/qontinui-types-drift.yml` runs `npm install` in
# qontinui-schemas as an explicit step, so the only lane that ever runs this
# codegen against a checkout WITHOUT node_modules is the local pre-push hook —
# and nothing runs that in CI. A freshly provisioned agent worktree is exactly
# that lane, and every push from one was refused with a raw
# ERR_MODULE_NOT_FOUND until the library under test landed.
#
# The property is two-directional and both directions are pinned:
#
#   * a resolvable environment is never disturbed          (rung 1 short-circuits)
#   * an unresolvable one is REPAIRED, not excused         (donor / npm rungs)
#   * a repair that does not actually work is never claimed (every rung is
#     confirmed by importing the real specifiers)
#   * a version-mismatched donor is never borrowed         (no silent toolchain swap)
#   * when nothing can be done the state is `unavailable`  (never a quiet pass)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/ts-codegen-deps.sh
. "$SCRIPT_DIR/lib/ts-codegen-deps.sh"

# The library derives a donor from `git rev-parse --git-common-dir`, and under a
# hook an inherited GIT_DIR overrides `git -C`. Clear it, or the git-worktree
# case below asks the REAL repository where its common dir is.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
      GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR 2>/dev/null || true

PASS=0
FAIL=0
SKIP=0
WORK=""

cleanup() { [ -n "$WORK" ] && rm -rf "$WORK"; }
trap cleanup EXIT

check() {
    local label="$1" want="$2" got="$3"
    if [ "$want" = "$got" ]; then
        printf '  ok   %s\n' "$label"
        PASS=$((PASS + 1))
    else
        printf '  FAIL %s\n       want: %s\n       got : %s\n' "$label" "$want" "$got"
        FAIL=$((FAIL + 1))
    fi
}
pass_note() { printf '  ok   %s\n' "$1"; PASS=$((PASS + 1)); }
fail_note() { printf '  FAIL %s\n' "$1"; FAIL=$((FAIL + 1)); }
skip_note() { printf '  SKIP %s\n' "$1"; SKIP=$((SKIP + 1)); }

# ── Fixtures ────────────────────────────────────────────────────────────────

# A fake node_modules holding the two packages compile_typescript.mjs imports,
# at the versions given. No `exports` map, deliberately: that leaves Node's
# legacy subpath resolution in play, which is what makes the deep
# `json-schema-to-typescript/dist/src/utils.js` import resolvable against a
# stub without reproducing the real package's export table. `"type": "module"`
# so the stubs offer real NAMED exports — the fixture script below imports them
# by name, exactly as the real compile_typescript.mjs does.
make_node_modules() { # <dir> <j2ts version> <prettier version>
    local nm="$1" jv="$2" pv="$3"
    mkdir -p "$nm/json-schema-to-typescript/dist/src" "$nm/prettier"
    printf '{"name":"json-schema-to-typescript","version":"%s","type":"module","main":"index.js"}\n' "$jv" \
        > "$nm/json-schema-to-typescript/package.json"
    printf 'export const compile = () => {};\n' > "$nm/json-schema-to-typescript/index.js"
    printf 'export const toSafeString = (s) => s;\n' > "$nm/json-schema-to-typescript/dist/src/utils.js"
    printf '{"name":"prettier","version":"%s","type":"module","main":"index.js"}\n' "$pv" \
        > "$nm/prettier/package.json"
    printf 'export const format = (s) => s;\n' > "$nm/prettier/index.js"
}

# A stand-in for a qontinui-schemas checkout: the pins, and a compile script
# that imports exactly what the real one imports (and proves it ran).
make_schemas() { # <dir> <pinned j2ts> <pinned prettier>
    local d="$1" jv="$2" pv="$3"
    mkdir -p "$d/scripts"
    printf '{"name":"@qontinui/shared-types","devDependencies":{"json-schema-to-typescript":"%s","prettier":"%s"}}\n' \
        "$jv" "$pv" > "$d/package.json"
    cat > "$d/scripts/compile_typescript.mjs" <<'MJS'
import { compile } from 'json-schema-to-typescript';
import { toSafeString } from 'json-schema-to-typescript/dist/src/utils.js';
import { format } from 'prettier';
console.log('compile_typescript ran');
MJS
}

fixture() { # fresh $WORK, plus a schemas checkout at $WORK/schemas
    cleanup
    WORK="$(mktemp -d -t ts-codegen-deps-test-XXXXXXXX)"
    make_schemas "$WORK/schemas" "1.2.3" "4.5.6"
    mkdir -p "$WORK/scratch"
}

resolve_here() { # <schemas dir>
    unset QONTINUI_TS_CODEGEN_NODE_MODULES
    ts_codegen_deps_resolve "$1" "$WORK/scratch"
}

if ! command -v node >/dev/null 2>&1; then
    printf 'node is not on PATH — every case here needs it. Nothing was tested.\n' >&2
    exit 1
fi

# ── 1. The sibling already has what it needs: nothing else may happen ───────

echo "-- rung: sibling --"
fixture
make_node_modules "$WORK/schemas/node_modules" "1.2.3" "4.5.6"
resolve_here "$WORK/schemas"
check "state is sibling" "sibling" "$TS_CODEGEN_DEPS_STATE"
check "runs the checkout's OWN script" \
    "$WORK/schemas/scripts/compile_typescript.mjs" "$TS_CODEGEN_DEPS_SCRIPT"
if [ -e "$WORK/scratch/shim" ]; then
    fail_note "the sibling rung built a shim it did not need"
else
    pass_note "no shim was built"
fi

# ── 2. Bare sibling + an explicitly named donor ─────────────────────────────

echo "-- rung: donor (explicit) --"
fixture
make_node_modules "$WORK/donor/node_modules" "1.2.3" "4.5.6"
QONTINUI_TS_CODEGEN_NODE_MODULES="$WORK/donor/node_modules" \
    ts_codegen_deps_resolve "$WORK/schemas" "$WORK/scratch"
check "state is donor" "donor" "$TS_CODEGEN_DEPS_STATE"
if [ -n "$TS_CODEGEN_DEPS_SCRIPT" ] && [ -f "$TS_CODEGEN_DEPS_SCRIPT" ]; then
    pass_note "a runnable script was produced"
else
    fail_note "no runnable script: '$TS_CODEGEN_DEPS_SCRIPT'"
fi
# The point of the rung is that the script actually RUNS. Assert that, not the
# resolver's own opinion of it.
if out="$(node "$TS_CODEGEN_DEPS_SCRIPT" 2>&1)"; then
    check "the resolved script executes" "compile_typescript ran" "$out"
else
    fail_note "the resolved script did not execute: $out"
fi
# The invariant the whole library exists to preserve.
if [ -e "$WORK/schemas/node_modules" ]; then
    fail_note "the donor rung wrote node_modules into the schemas checkout"
else
    pass_note "the schemas checkout was not written to"
fi

# ── 3. Bare sibling + a donor derived from git, not from a guessed layout ───

echo "-- rung: donor (derived from --git-common-dir) --"
if ! command -v git >/dev/null 2>&1; then
    skip_note "git is not on PATH"
else
    fixture
    # $WORK/schemas becomes a real repo; a LINKED WORKTREE of it stands in for
    # the coord-allocated sibling, and only the primary gets node_modules.
    git -C "$WORK/schemas" init -q
    git -C "$WORK/schemas" config user.email t@example.com
    git -C "$WORK/schemas" config user.name t
    git -C "$WORK/schemas" add -A
    git -C "$WORK/schemas" commit -qm init
    make_node_modules "$WORK/schemas/node_modules" "1.2.3" "4.5.6"
    if git -C "$WORK/schemas" worktree add -q -b wt "$WORK/wt" >/dev/null 2>&1; then
        resolve_here "$WORK/wt"
        check "state is donor" "donor" "$TS_CODEGEN_DEPS_STATE"
        case "$TS_CODEGEN_DEPS_DETAIL" in
            *"$WORK/schemas/node_modules"*)
                pass_note "borrowed the PRIMARY checkout's node_modules" ;;
            *)
                fail_note "did not name the primary's node_modules: $TS_CODEGEN_DEPS_DETAIL" ;;
        esac
        if [ -e "$WORK/wt/node_modules" ]; then
            fail_note "wrote node_modules into the worktree checkout"
        else
            pass_note "the worktree checkout was not written to"
        fi
    else
        skip_note "git worktree add failed in this environment"
    fi
fi

# ── 4. A version-mismatched donor is never borrowed ─────────────────────────

echo "-- rung: donor version gate --"
fixture
make_node_modules "$WORK/donor/node_modules" "9.9.9" "4.5.6"
# A refusing `npm` shadows the real one, so the npm rung cannot rescue the
# mismatch and the verdict is decided by the version gate alone. Shadowing
# rather than stripping the PATH: on every box here `npm` and `node` live in
# the SAME directory, so removing npm's directory removes node's too and the
# ladder never reaches the rung under test.
make_refusing_npm() { # <dir>
    mkdir -p "$1"
    printf '#!/usr/bin/env bash\necho "no registry in this test" >&2\nexit 1\n' > "$1/npm"
    chmod +x "$1/npm"
}
make_refusing_npm "$WORK/fakebin"
QONTINUI_TS_CODEGEN_NODE_MODULES="$WORK/donor/node_modules" \
PATH="$WORK/fakebin:$PATH" \
    ts_codegen_deps_resolve "$WORK/schemas" "$WORK/scratch"
check "state is unavailable" "unavailable" "$TS_CODEGEN_DEPS_STATE"
check "no script is offered" "" "$TS_CODEGEN_DEPS_SCRIPT"
case "$TS_CODEGEN_DEPS_TRIED" in
    *"do not match the pins"*) pass_note "the diagnostic names the version mismatch" ;;
    *) fail_note "the diagnostic does not name the mismatch: $TS_CODEGEN_DEPS_TRIED" ;;
esac

# ── 5. Nothing available at all: loud unavailable, never a quiet pass ───────

echo "-- rung: unavailable --"
fixture
make_refusing_npm "$WORK/fakebin"
PATH="$WORK/fakebin:$PATH" resolve_here "$WORK/schemas"
check "state is unavailable" "unavailable" "$TS_CODEGEN_DEPS_STATE"
case "$TS_CODEGEN_DEPS_TRIED" in
    *"does not resolve from"*) pass_note "the diagnostic names the sibling that could not resolve" ;;
    *) fail_note "the diagnostic is empty or unhelpful: $TS_CODEGEN_DEPS_TRIED" ;;
esac
case "$TS_CODEGEN_DEPS_TRIED" in
    *"npm install' in the shim exited"*) pass_note "the diagnostic names the failed install too" ;;
    *) fail_note "the npm rung's failure is not reported: $TS_CODEGEN_DEPS_TRIED" ;;
esac

echo "-- rung: no node at all --"
fixture
PATH="/nonexistent" resolve_here "$WORK/schemas"
check "state is unavailable" "unavailable" "$TS_CODEGEN_DEPS_STATE"
case "$TS_CODEGEN_DEPS_TRIED" in
    *"node is not on PATH"*) pass_note "the diagnostic names the absent interpreter" ;;
    *) fail_note "the diagnostic does not name node: $TS_CODEGEN_DEPS_TRIED" ;;
esac

# ── 6. The npm rung (opt-in: it is the only case needing a registry) ────────

echo "-- rung: npm --"
if [ "${QONTINUI_TS_CODEGEN_DEPS_TEST_NPM:-0}" != "1" ]; then
    skip_note "npm rung not exercised (set QONTINUI_TS_CODEGEN_DEPS_TEST_NPM=1; needs npm + a registry or a warm cache)"
elif ! command -v npm >/dev/null 2>&1; then
    skip_note "npm is not on PATH"
else
    fixture
    # Real pins, so npm has something it can actually fetch.
    make_schemas "$WORK/schemas" "15.0.4" "3.9.6"
    resolve_here "$WORK/schemas"
    check "state is npm" "npm" "$TS_CODEGEN_DEPS_STATE"
    if [ -n "$TS_CODEGEN_DEPS_SCRIPT" ] && out="$(node "$TS_CODEGEN_DEPS_SCRIPT" 2>&1)"; then
        check "the resolved script executes against the real packages" \
            "compile_typescript ran" "$out"
    else
        fail_note "the resolved script did not execute: ${out:-<none>}"
    fi
    if [ -e "$WORK/schemas/node_modules" ]; then
        fail_note "the npm rung installed into the schemas checkout"
    else
        pass_note "the schemas checkout was not written to"
    fi
fi

echo
if [ "$SKIP" -gt 0 ]; then
    printf '%d passed, %d failed, %d SKIPPED (arms not exercised in this environment)\n' \
        "$PASS" "$FAIL" "$SKIP"
else
    printf '%d passed, %d failed\n' "$PASS" "$FAIL"
fi
[ "$FAIL" -eq 0 ]
