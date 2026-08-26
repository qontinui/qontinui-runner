#!/usr/bin/env bash
#
# Tests for `lib/push-range.sh` — the base-ref cascade that decides what
# "the pushed range" means — and for the two behaviours `cargo-prepush.sh`
# builds on top of it: the src-tauri/ diff scoping, and the workspace preflight
# that declines when cargo cannot load the manifest tree.
#
# Run: bash .pre-commit-hooks/push-range-test.sh
#
# A SIBLING of gen-events-drift-attribution-test.sh rather than growth of it:
# that file's header scopes it to `lib/gen-events-attribution.sh` and its
# helpers are built around attribution state (ATTRIBUTION_STATE,
# ATTRIBUTION_BASE_REF). One test file per lib file keeps the mapping trivial.
# The throwaway-git-repo harness is COPIED from it, which is the point of
# citing it as a pattern.
#
# Everything here builds real git repos with a real `origin` under $TMPDIR.
# The diff-scope cases need NO Rust toolchain: `cargo` is replaced by a
# recording shim on PATH, so "did the gate run?" is answered by whether the
# shim was invoked rather than by paying a build. The last two cases DO need a
# real `cargo` (they assert on cargo's own manifest-load error text) and are
# SKIPPED WITH A STATED REASON when it is absent — never silently passed.
#
# THE REGRESSION THIS FILE EXISTS FOR
#
# `cargo-prepush.sh` used to scope its diff on a bare `git rev-parse @{u}`. An
# allocated agent worktree's branch has no upstream until its first push
# completes, so the check never fired there and every worktree push — including
# TS-only ones — paid the full Rust gate. The first case below is that bug.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/push-range.sh
. "$SCRIPT_DIR/lib/push-range.sh"

PREPUSH="$SCRIPT_DIR/cargo-prepush.sh"

# MUST come before the first `git`. Git exports GIT_DIR (and friends) to every
# hook it runs, and those OVERRIDE `git -C` — so without this, a run from a
# real pre-push hook aims every fixture below at the REAL repository. That is
# not hypothetical: it happened to this file's sibling the first time it ran
# from a hook, and it committed its fixtures onto the branch being pushed.
# `unset` (not a subshell) so the child processes we exec below are clean too.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
    GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR GIT_PREFIX \
    GIT_INTERNAL_SUPER_PREFIX GIT_CONFIG GIT_CONFIG_COUNT \
    GIT_CONFIG_GLOBAL GIT_CONFIG_SYSTEM GIT_NAMESPACE \
    GIT_INDEX_VERSION GIT_QUARANTINE_PATH GIT_PUSH_CERT \
    GIT_REFLOG_ACTION
# The hook reads these from the environment; a developer running the tests with
# one exported must not silently change what is under test.
unset QONTINUI_PREPUSH_SKIP QONTINUI_PREPUSH_SKIP_ALL QONTINUI_PREPUSH_STRICT

PASS=0
FAIL=0
SKIP=0
WORK=""
UPSTREAM=""
SHIM_LOG=""

FIXTURE_ROOTS=()
cleanup() {
    local root
    for root in "${FIXTURE_ROOTS[@]:-}"; do
        [ -n "$root" ] || continue
        # `chmod -R` first: git makes objects read-only, which blocks `rm` on
        # some filesystems (notably a Windows checkout).
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

# A case this environment could not set up. Counted and reported in the summary
# rather than printed and forgotten: an arm that silently stops being exercised
# reads exactly like an arm that passes.
skip_note() { printf '  SKIP %s\n' "$1"; SKIP=$((SKIP + 1)); }

# --------------------------------------------------------------------------
# Fixture: a repo with an `origin` remote whose `main` is the upstream of the
# local branch — the shape every real push has. A real clone rather than a
# faked remote-tracking ref, so `merge-base` resolves exactly as it does live.
# --------------------------------------------------------------------------
fixture() {
    local root
    # Hard-fail rather than continue: without this a failed `mktemp` leaves
    # $WORK pointing at the PREVIOUS fixture, and every later case then reports
    # a plausible-looking verdict about the wrong repo.
    if ! root="$(mktemp -d -t push-range-XXXXXX)" || [ ! -d "$root" ]; then
        printf '  FATAL could not create a fixture directory\n' >&2
        exit 1
    fi
    FIXTURE_ROOTS+=("$root")
    UPSTREAM="$root/upstream"
    WORK="$root/work"
    SHIM_LOG="$root/cargo-invocations"

    git init --quiet --initial-branch=main "$UPSTREAM"
    git -C "$UPSTREAM" config user.email t@example.com
    git -C "$UPSTREAM" config user.name t
    git -C "$UPSTREAM" config core.autocrlf false
    # Fixture commits must not run the developer machine's git hooks: a global
    # core.hooksPath would aim these throwaway repos at this repo's own
    # pre-commit install, and a hook failing on a two-line fixture would read
    # as a cascade bug.
    git -C "$UPSTREAM" config core.hooksPath "$UPSTREAM/.git/no-hooks"
    mkdir -p "$UPSTREAM/src-tauri/src" "$UPSTREAM/src"
    printf '// base\n'   > "$UPSTREAM/src-tauri/src/lib.rs"
    printf '// ui\n'     > "$UPSTREAM/src/app.ts"
    printf '[workspace]\n' > "$UPSTREAM/Cargo.toml"
    printf '# lock\n'      > "$UPSTREAM/Cargo.lock"
    git -C "$UPSTREAM" add -A >/dev/null
    git -C "$UPSTREAM" commit --quiet -m base

    # `--config` and not a post-clone `git config`: the setting has to be in
    # force for the CHECKOUT, or the tree lands with CRLF and every file then
    # reads as modified against the index.
    git clone --quiet --config core.autocrlf=false "$UPSTREAM" "$WORK"
    git -C "$WORK" config user.email t@example.com
    git -C "$WORK" config user.name t
    git -C "$WORK" config core.hooksPath "$WORK/.git/no-hooks"

    # The fixture is only a fixture if git agrees. Compared through
    # `cd … && pwd -P` on BOTH sides rather than by string: on Git Bash $WORK is
    # an MSYS path while git answers with a Windows one, and those name the
    # same directory.
    local want got toplevel
    want="$(cd "$WORK" && pwd -P)"
    toplevel="$(git -C "$WORK" rev-parse --show-toplevel 2>/dev/null || true)"
    got=""
    [ -n "$toplevel" ] && got="$(cd "$toplevel" 2>/dev/null && pwd -P)"
    if [ -z "$got" ] || [ "$want" != "$got" ]; then
        printf '  FATAL fixture escaped: git -C %s resolves to %s\n' \
            "$want" "${got:-<unresolvable>}" >&2
        printf '        A git environment variable is overriding -C. Refusing to run.\n' >&2
        exit 1
    fi

    # A recording `cargo` shim. This is what makes "did the gate run?" answerable
    # without a Rust toolchain, a schemas checkout, or a ~minute build: the hook
    # under test shells out to `cargo`, so intercepting that call is a faithful
    # observation of the decision, not a simulation of it.
    mkdir -p "$root/bin"
    cat > "$root/bin/cargo" <<'SHIM'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$CARGO_SHIM_LOG"
exit 0
SHIM
    chmod +x "$root/bin/cargo"
    SHIM_BIN="$root/bin"
    : > "$SHIM_LOG"
}

commit_change() {
    local path="$1" text="$2"
    mkdir -p "$WORK/$(dirname "$path")"
    printf '%s\n' "$text" >> "$WORK/$path"
    git -C "$WORK" add -A >/dev/null
    git -C "$WORK" commit --quiet -m "touch $path"
}

# Run the real hook in the fixture, with the recording shim ahead of any real
# cargo on PATH. Captures combined output; sets HOOK_RC and HOOK_OUT.
run_prepush() {
    HOOK_OUT="$(cd "$WORK" && CARGO_SHIM_LOG="$SHIM_LOG" PATH="$SHIM_BIN:$PATH" \
        bash "$PREPUSH" 2>&1)"
    HOOK_RC=$?
}

cargo_ran() {
    [ -s "$SHIM_LOG" ] && echo yes || echo no
}

echo "push-range / cargo-prepush diff scoping"
echo "  -- the base-ref cascade itself --"

# Fallback 1: the branch's own upstream.
fixture
check "with an upstream, that upstream is the base" "origin/main" "$(push_base_ref "$WORK")"

# Fallback 2: no upstream, but the remote publishes a default branch. Pointed at
# a non-`main` name so the resolved ref PROVES which fallback fired — with
# origin/HEAD -> origin/main the answer is the same string as fallback 3.
fixture
git -C "$WORK" update-ref refs/remotes/origin/trunk refs/remotes/origin/main
git -C "$WORK" symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/trunk
git -C "$WORK" update-ref -d refs/remotes/origin/main
git -C "$WORK" branch --unset-upstream >/dev/null 2>&1
check "with no upstream, origin/HEAD is the base" "origin/trunk" "$(push_base_ref "$WORK")"

# Fallback 3: no upstream and no published default branch.
fixture
git -C "$WORK" branch --unset-upstream >/dev/null 2>&1
# `symbolic-ref --delete`, never `update-ref -d`: the latter DEREFERENCES, so it
# would delete refs/remotes/origin/main and leave nothing for fallback 3.
git -C "$WORK" symbolic-ref --delete refs/remotes/origin/HEAD >/dev/null 2>&1
check "with neither, origin/main is the base" "origin/main" "$(push_base_ref "$WORK")"

# Nothing resolvable: returns NON-ZERO and prints nothing. The function takes no
# position on what the caller should do about it — that neutrality is what lets
# gen-events attribution fail closed while this hook fails open.
fixture
git -C "$WORK" branch --unset-upstream >/dev/null 2>&1
git -C "$WORK" remote remove origin >/dev/null 2>&1
git -C "$WORK" update-ref -d refs/remotes/origin/main >/dev/null 2>&1
git -C "$WORK" symbolic-ref --delete refs/remotes/origin/HEAD >/dev/null 2>&1
out="$(push_base_ref "$WORK")"; rc=$?
check "with no remote at all the cascade returns non-zero" "1" "$rc"
check "and prints nothing" "" "$out"

echo "  -- the Defect A regression: no upstream must not defeat diff scoping --"

# THE case. A fresh agent worktree branch: no upstream, TS-only diff. Before
# 2026-08-26 the `@{u}` lookup failed here, the skip block never evaluated, and
# the hook ran the whole Rust gate for a diff containing no Rust.
fixture
git -C "$WORK" branch --unset-upstream >/dev/null 2>&1
commit_change "src/app.ts" "// my ui change"
run_prepush
check "no upstream + no src-tauri/ change -> gate SKIPPED (exit 0)" "0" "$HOOK_RC"
check "no upstream + no src-tauri/ change -> cargo never invoked" "no" "$(cargo_ran)"
case "$HOOK_OUT" in
    *"skipping cargo gate"*) pass_note "and it says which base ref it scoped against" ;;
    *) fail_note "expected a 'skipping cargo gate' line, got: $HOOK_OUT" ;;
esac

# The other direction: a real Rust change with no upstream must still be gated.
# A scoping fix that skipped here would be worse than the bug it replaced.
fixture
git -C "$WORK" branch --unset-upstream >/dev/null 2>&1
commit_change "src-tauri/src/lib.rs" "// my rust change"
run_prepush
check "no upstream + a src-tauri/ change -> gate ATTEMPTED" "yes" "$(cargo_ran)"

echo "  -- the arms that already worked, pinned so the fix cannot regress them --"

fixture
commit_change "src/app.ts" "// ui"
run_prepush
check "upstream present + no src-tauri/ change -> gate SKIPPED" "0" "$HOOK_RC"
check "upstream present + no src-tauri/ change -> cargo never invoked" "no" "$(cargo_ran)"

fixture
commit_change "src-tauri/src/lib.rs" "// rust"
run_prepush
check "upstream present + a src-tauri/ change -> gate ATTEMPTED" "yes" "$(cargo_ran)"

# Conservative fallback preserved: when the cascade can answer nothing, RUN the
# gate. Fails OPEN by design — an unscoped gate costs latency, a wrongly skipped
# one costs the signal.
fixture
git -C "$WORK" branch --unset-upstream >/dev/null 2>&1
git -C "$WORK" remote remove origin >/dev/null 2>&1
git -C "$WORK" update-ref -d refs/remotes/origin/main >/dev/null 2>&1
git -C "$WORK" symbolic-ref --delete refs/remotes/origin/HEAD >/dev/null 2>&1
commit_change "src/app.ts" "// ui"
run_prepush
check "no resolvable base ref -> gate RUNS anyway (fail open)" "yes" "$(cargo_ran)"
case "$HOOK_OUT" in
    *"to scope the diff against"*) pass_note "and it says why it could not scope" ;;
    *) fail_note "expected a note explaining the unscoped run, got: $HOOK_OUT" ;;
esac

echo "  -- Defect B: decline honestly when cargo cannot load the workspace --"

# These two assert on cargo's OWN manifest-load error text, so they need a real
# cargo — but no Rust toolchain beyond it and no compilation: `cargo metadata`
# fails before it resolves anything.
broken_workspace_fixture() {
    fixture
    git -C "$WORK" branch --unset-upstream >/dev/null 2>&1
    git -C "$WORK" remote remove origin >/dev/null 2>&1
    git -C "$WORK" update-ref -d refs/remotes/origin/main >/dev/null 2>&1
    git -C "$WORK" symbolic-ref --delete refs/remotes/origin/HEAD >/dev/null 2>&1
    cat > "$WORK/Cargo.toml" <<'TOML'
[workspace]
members = ["src-tauri"]
resolver = "2"
TOML
    mkdir -p "$WORK/src-tauri/src"
    cat > "$WORK/src-tauri/Cargo.toml" <<'TOML'
[package]
name = "fixture-app"
version = "0.0.0"
edition = "2021"

[dependencies]
# The defect, reproduced: a path dependency on a FILESYSTEM SIBLING repo that
# is not present in this bundle. Exactly the shape of
# `qontinui-types = { path = "../../qontinui-schemas/rust" }`.
qontinui-types = { path = "../../qontinui-schemas-absent/rust" }
TOML
    printf 'fn main() {}\n' > "$WORK/src-tauri/src/main.rs"
    git -C "$WORK" add -A >/dev/null
    git -C "$WORK" commit --quiet -m "a workspace with an unresolvable path dep"
}

# Run WITHOUT the shim: the real cargo is the thing under test here.
run_prepush_real_cargo() {
    HOOK_OUT="$(cd "$WORK" && bash "$PREPUSH" 2>&1)"
    HOOK_RC=$?
}

if ! command -v cargo >/dev/null 2>&1; then
    skip_note "unresolvable path dep declines: no 'cargo' on PATH, and this arm asserts on cargo's own manifest-load error text"
    skip_note "QONTINUI_PREPUSH_STRICT=1 turns the decline into a failure: same reason"
else
    broken_workspace_fixture
    run_prepush_real_cargo
    check "an unresolvable path dep DECLINES rather than blocking (exit 0)" "0" "$HOOK_RC"
    case "$HOOK_OUT" in
        *"CANNOT RUN THE CARGO GATE"*) pass_note "the decline is [pre-push]-framed and typed" ;;
        *) fail_note "expected a typed decline, got: $HOOK_OUT" ;;
    esac
    case "$HOOK_OUT" in
        *"MANIFEST-LOAD failure"*) pass_note "it names the cause (manifest load, not a lint)" ;;
        *) fail_note "the decline must name the cause" ;;
    esac
    case "$HOOK_OUT" in
        *"qontinui-schemas"*) pass_note "it names the remedy (materialize the sibling checkout)" ;;
        *) fail_note "the decline must name the remedy" ;;
    esac
    case "$HOOK_OUT" in
        *"QONTINUI_PREPUSH_STRICT=1"*) pass_note "it names the strict switch" ;;
        *) fail_note "the decline must name QONTINUI_PREPUSH_STRICT=1" ;;
    esac
    # The one place this message must NOT copy its gen-events-drift template:
    # CI genuinely runs `cargo fmt -- --check` and `cargo clippy` in the required
    # context, so declining here costs latency, NOT coverage. Telling a developer
    # they are unprotected when they are not is its own defect.
    case "$HOOK_OUT" in
        *"CI STILL GATES BOTH HALVES"*) pass_note "it states the TRUE coverage conclusion (CI still gates both halves)" ;;
        *) fail_note "the decline must say CI still gates both halves" ;;
    esac
    # And it must not have died on a raw cargo dump under `set -e`.
    case "$HOOK_OUT" in
        *"[pre-push]"*) pass_note "output is framed, not a bare cargo dump" ;;
        *) fail_note "output is not [pre-push]-framed: $HOOK_OUT" ;;
    esac

    broken_workspace_fixture
    HOOK_OUT="$(cd "$WORK" && QONTINUI_PREPUSH_STRICT=1 bash "$PREPUSH" 2>&1)"
    HOOK_RC=$?
    check "QONTINUI_PREPUSH_STRICT=1 turns the decline into a hard failure" "1" "$HOOK_RC"
    case "$HOOK_OUT" in
        *"QONTINUI_PREPUSH_STRICT=1 — treating this as a failure"*)
            pass_note "and it says the strict switch is what blocked" ;;
        *) fail_note "strict mode must say why it blocked" ;;
    esac
fi

echo
if [ "$SKIP" -gt 0 ]; then
    printf '%d passed, %d failed, %d SKIPPED (arms not exercised in this environment)\n' \
        "$PASS" "$FAIL" "$SKIP"
else
    printf '%d passed, %d failed\n' "$PASS" "$FAIL"
fi
[ "$FAIL" -eq 0 ]
