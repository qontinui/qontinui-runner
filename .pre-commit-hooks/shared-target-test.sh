#!/usr/bin/env bash
#
# Tests for `lib/shared-target.sh` — "which target dir should a pre-push build
# write into?" — and for the two things that make its answer reach the SECOND
# hook: `gen-events-drift.sh` sourcing it, and `generate_types.sh` honouring the
# `QONTINUI_SCHEMAS_JSON` override that keeps a now-shared target dir from
# becoming a collision point.
#
# Run: bash .pre-commit-hooks/shared-target-test.sh
#
# One test file per lib file, the convention `push-range-test.sh` states in its
# own header. `cargo-prepush.sh`'s use of this library is already covered there
# (the "a linked worktree borrows the shared target" section) and is not
# duplicated; what is new here is the library standing alone with a caller's own
# log prefix, and the gen-events side of it.
#
# THE REGRESSION THIS FILE EXISTS FOR
#
# qontinui-runner#1556 resolved the shared target INSIDE `cargo-prepush.sh` and
# exported it there. The pre-push shim runs each hook as its own `bash`, so
# `gen-events-drift.sh` never saw it and kept cold-building a whole dependency
# tree — finding 33d6f2d8 (b) measured 5.2 GB for ONE push across the two hooks
# together. An export that reaches one process out of two is the shape this
# suite watches for.
#
# No Rust toolchain: a recording `cargo` shim on PATH answers "what did the
# build see?" without paying a build.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LIB="$SCRIPT_DIR/lib/shared-target.sh"

# MUST come before the first `git` — see push-range-test.sh's own note.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
    GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR GIT_PREFIX \
    GIT_INTERNAL_SUPER_PREFIX GIT_CONFIG GIT_CONFIG_COUNT \
    GIT_CONFIG_GLOBAL GIT_CONFIG_SYSTEM GIT_NAMESPACE \
    GIT_INDEX_VERSION GIT_QUARANTINE_PATH GIT_PUSH_CERT \
    GIT_REFLOG_ACTION
# The resolution reads these; an exported one would decide the cases below
# instead of the fixture.
unset CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR QONTINUI_PREPUSH_CARGO_GUARD QONTINUI_ROOT

PASS=0
FAIL=0
FIXTURE_ROOTS=()
cleanup() {
    local root
    for root in "${FIXTURE_ROOTS[@]:-}"; do
        [ -n "$root" ] || continue
        chmod -R u+w "$root" 2>/dev/null || true
        rm -rf "$root"
    done
}
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

# A primary checkout plus a linked worktree of it, and a stub cargo-guard.sh
# that answers the resolve-only question the way the real one does. The stub
# records that it was asked, so "did the caller consult the guard?" is
# observable rather than inferred.
wt_fixture() {
    local root
    if ! root="$(mktemp -d -t shared-target-XXXXXX)" || [ ! -d "$root" ]; then
        printf '  FATAL could not create a fixture directory\n' >&2
        exit 1
    fi
    FIXTURE_ROOTS+=("$root")
    PRIMARY="$root/primary"
    WT="$root/wt"
    SHARED_TARGET="$root/shared-target"
    GUARD_LOG="$root/guard-invocations"
    STUB_GUARD="$root/cargo-guard-stub.sh"

    git init --quiet --initial-branch=main "$PRIMARY"
    git -C "$PRIMARY" config user.email t@example.com
    git -C "$PRIMARY" config user.name t
    git -C "$PRIMARY" config core.hooksPath "$PRIMARY/.git/no-hooks"
    printf '// base\n' > "$PRIMARY/lib.rs"
    git -C "$PRIMARY" add -A >/dev/null
    git -C "$PRIMARY" commit --quiet -m base
    git -C "$PRIMARY" worktree add --quiet -b wt-branch "$WT" >/dev/null 2>&1

    cat > "$STUB_GUARD" <<STUB
#!/usr/bin/env bash
printf '%s RESOLVE_ONLY=%s\n' "\$*" "\${CARGO_GUARD_RESOLVE_ONLY:-}" >> "$GUARD_LOG"
exit_code="\${STUB_GUARD_EXIT:-0}"
[ "\$exit_code" = "0" ] || exit "\$exit_code"
printf 'RUNNER_ROOT=%s\n' "\$PWD"
printf 'TARGET_DIR=%s\n' "$SHARED_TARGET"
STUB
    : > "$GUARD_LOG"
}

# Source the library in a CHILD shell and report what it resolved, so an export
# cannot leak from one case into the next. Prints "<exported value>|<output>".
# ⚠️ The function is called DIRECTLY and its output redirected to a file, never
# wrapped in `$( )`. A command substitution runs in a subshell, so the `export`
# — the entire point of the function — would be discarded there and every case
# below would read as "exported nothing". The real hooks call it directly too,
# which is why that harness bug looked like a library defect for one run.
resolve_in() {
    local root="$1" tag="$2"
    shift 2
    local outfile
    outfile="$(mktemp)"
    env -u CARGO_TARGET_DIR "$@" bash -c '
        . "$1"
        resolve_shared_target "$2" "$3" > "$4"
        # Read the value back through a CHILD, because EXPORTING is the whole
        # job. A plain "${CARGO_TARGET_DIR:-}" here still sees a variable that
        # was merely ASSIGNED, so dropping the `export` would leave every case
        # below green while cargo — a child process — saw nothing.
        printf "%s|%s" "$(printenv CARGO_TARGET_DIR || true)" "$(cat "$4")"
    ' _ "$LIB" "$root" "$tag" "$outfile"
    rm -f "$outfile"
}

echo "shared-target resolution"

wt_fixture
got="$(resolve_in "$PRIMARY" "[pre-push]" QONTINUI_PREPUSH_CARGO_GUARD="$STUB_GUARD")"
check "the primary checkout is a silent no-op — nothing exported, nothing said" "|" "$got"
check "and the guard is never asked" "" "$(cat "$GUARD_LOG")"

wt_fixture
got="$(resolve_in "$WT" "[pre-push]" QONTINUI_PREPUSH_CARGO_GUARD="$STUB_GUARD")"
check "a linked worktree exports the guard's target" "$SHARED_TARGET" "${got%%|*}"
check "and the guard is asked in resolve-only mode" "check RESOLVE_ONLY=1" "$(cat "$GUARD_LOG")"

# THE property this library exists for: the SAME answer under a second caller's
# framing. A resolution that only ever prints "[pre-push]" would be a copy, not
# a shared library, and the gen-events output would lie about who spoke.
wt_fixture
got="$(resolve_in "$WT" "[gen-events-drift]" QONTINUI_PREPUSH_CARGO_GUARD="$STUB_GUARD")"
check "a second caller gets the same target" "$SHARED_TARGET" "${got%%|*}"
case "${got#*|}" in
    "[gen-events-drift] linked worktree — reusing the shared target CARGO_TARGET_DIR=$SHARED_TARGET"*)
        pass_note "and the message carries THAT caller's prefix" ;;
    *) fail_note "expected a [gen-events-drift]-framed message, got: ${got#*|}" ;;
esac

wt_fixture
got="$(resolve_in "$WT" "[pre-push]" QONTINUI_PREPUSH_CARGO_GUARD="$STUB_GUARD" CARGO_TARGET_DIR=/caller/chose/this)"
check "a caller-set CARGO_TARGET_DIR wins" "/caller/chose/this" "${got%%|*}"
check "and the guard is NOT asked" "" "$(cat "$GUARD_LOG")"

wt_fixture
got="$(resolve_in "$WT" "[pre-push]" QONTINUI_PREPUSH_CARGO_GUARD="$STUB_GUARD" STUB_GUARD_EXIT=2)"
check "a guard that refuses exports nothing" "" "${got%%|*}"
case "${got#*|}" in
    *"could not resolve a target"*) pass_note "and says the target could not be resolved" ;;
    *) fail_note "expected a one-line note, got: ${got#*|}" ;;
esac

wt_fixture
printf '#!/usr/bin/env bash\nprintf "RUNNER_ROOT=%%s\\n" "$PWD"\n' > "$STUB_GUARD"
got="$(resolve_in "$WT" "[pre-push]" QONTINUI_PREPUSH_CARGO_GUARD="$STUB_GUARD")"
check "a guard that names no TARGET_DIR invents none" "" "${got%%|*}"
case "${got#*|}" in
    *"printed no TARGET_DIR"*) pass_note "and says the guard named no target" ;;
    *) fail_note "expected a note that no TARGET_DIR was printed, got: ${got#*|}" ;;
esac

wt_fixture
got="$(resolve_in "$WT" "[pre-push]" QONTINUI_PREPUSH_CARGO_GUARD="$(dirname "$STUB_GUARD")/no-such-guard.sh")"
check "no guard anywhere exports nothing" "" "${got%%|*}"
case "${got#*|}" in
    *"no cargo-guard.sh to resolve the shared target"*) pass_note "and says no guard was found" ;;
    *) fail_note "expected a note that no guard was found, got: ${got#*|}" ;;
esac

echo "  -- the export has to REACH generate_types.sh --"

# A recording `cargo` shim, so this asks "what did the build see?" without a
# Rust toolchain and without ever compiling anything.
gen_fixture() {
    local root
    root="$(mktemp -d -t gen-types-XXXXXX)" || { printf '  FATAL mktemp\n' >&2; exit 1; }
    FIXTURE_ROOTS+=("$root")
    GEN_ROOT="$root"
    mkdir -p "$root/bin" "$root/target" "$root/ts" "$root/py"
    cat > "$root/bin/cargo" <<'SHIM'
#!/usr/bin/env bash
printf '%s | CARGO_TARGET_DIR=%s\n' "$*" "${CARGO_TARGET_DIR:-<unset>}" >> "$CARGO_SHIM_LOG"
exit 0
SHIM
    chmod +x "$root/bin/cargo"
    GEN_LOG="$root/cargo-invocations"
    : > "$GEN_LOG"
}

# Run the REAL generate_types.sh with the shim. It exits non-zero later (no
# node codegen script), which is fine and deliberate: everything asserted here
# happens before that. CARGO_TARGET_DIR and every output dir are pinned into
# the fixture so nothing is written into this checkout.
run_generate_types() {
    ( cd "$REPO_ROOT" \
      && CARGO_SHIM_LOG="$GEN_LOG" PATH="$GEN_ROOT/bin:$PATH" \
         CARGO_TARGET_DIR="$GEN_ROOT/target" \
         QONTINUI_TS_OUT_DIR="$GEN_ROOT/ts" QONTINUI_PY_OUT_DIR="$GEN_ROOT/py" \
         "$@" bash src-tauri/scripts/generate_types.sh --ts-only ) >/dev/null 2>&1
}

gen_fixture
run_generate_types env
check "generate_types.sh's build inherits CARGO_TARGET_DIR" \
    "$GEN_ROOT/target" "$(sed -n 's/.* | CARGO_TARGET_DIR=//p' "$GEN_LOG" | sort -u)"
check "and by default the schema export lands inside it" \
    "yes" "$([ -f "$GEN_ROOT/target/schemas.json" ] && echo yes || echo no)"

# THE collision this override closes. Once every worktree borrows ONE shared
# target, the default path above is the SAME file for all of them, and cargo's
# lock covers the build, not this redirect — two concurrent pushes would diff
# their bindings against each other's schema. `gen-events-drift.sh` therefore
# points it at its own per-run scratch dir.
gen_fixture
run_generate_types env QONTINUI_SCHEMAS_JSON="$GEN_ROOT/private-schemas.json"
check "QONTINUI_SCHEMAS_JSON moves the export out of the shared target" \
    "yes" "$([ -f "$GEN_ROOT/private-schemas.json" ] && echo yes || echo no)"
check "and nothing is left in the shared target to collide on" \
    "no" "$([ -f "$GEN_ROOT/target/schemas.json" ] && echo yes || echo no)"

echo "  -- gen-events-drift.sh is actually wired to both --"

# Structural, and deliberately so: a full functional run of the drift hook
# needs a schemas checkout, node, and its codegen dependencies, none of which
# this toolchain-free suite has. The FUNCTIONAL evidence lives one level down
# — every arm above exercises the library and the script this hook calls — and
# a live run from a real allocated worktree is recorded in the PR. What is
# pinned here is the join between them, which is exactly what #1556 got wrong.
HOOK="$SCRIPT_DIR/gen-events-drift.sh"
if grep -q 'lib/shared-target\.sh' "$HOOK"; then
    pass_note "gen-events-drift.sh sources lib/shared-target.sh"
else
    fail_note "gen-events-drift.sh does not source lib/shared-target.sh"
fi
if grep -q 'resolve_shared_target "\$RUNNER_DIR"' "$HOOK"; then
    pass_note "and calls it for its own repo root"
else
    fail_note "gen-events-drift.sh does not call resolve_shared_target for \$RUNNER_DIR"
fi
if grep -q 'QONTINUI_SCHEMAS_JSON=' "$HOOK"; then
    pass_note "and points the schema export at its own scratch dir"
else
    fail_note "gen-events-drift.sh does not set QONTINUI_SCHEMAS_JSON — the shared target is a collision point"
fi

echo
printf '%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
