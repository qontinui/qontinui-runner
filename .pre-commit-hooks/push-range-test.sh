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
# The shared-target resolution reads these too: an exported CARGO_TARGET_DIR,
# guard override or workspace root would decide the linked-worktree cases below
# instead of the fixture.
unset CARGO_TARGET_DIR QONTINUI_PREPUSH_CARGO_GUARD QONTINUI_ROOT
# The pushed-range resolution reads these. A developer running the tests from
# inside a real pre-commit pre-push invocation would otherwise have the RANGE
# under test decided by their own push.
unset PRE_COMMIT_FROM_REF PRE_COMMIT_TO_REF

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
    mkdir -p "$UPSTREAM/src-tauri/src" "$UPSTREAM/src" \
        "$UPSTREAM/crates/thing/src" "$UPSTREAM/src/components/app"
    # An in-repo path dependency and a build-script input: `cd src-tauri &&
    # cargo clippy` compiles the first and build.rs fatally reads the second,
    # so both are gate inputs that live OUTSIDE src-tauri/.
    printf '// dep\n' > "$UPSTREAM/crates/thing/src/lib.rs"
    printf 'export const VALID_TAB_IDS = [];\n' > "$UPSTREAM/src/components/app/tab-types.ts"
    cat > "$UPSTREAM/src-tauri/build.rs" <<'BUILDRS'
fn main() {
    const TAB_TYPES_TS: &str = "../src/components/app/tab-types.ts";
    println!("cargo:rerun-if-changed={TAB_TYPES_TS}");
}
BUILDRS
    printf '// base\n'   > "$UPSTREAM/src-tauri/src/lib.rs"
    # An include_str!-style markdown body, so a content-only edit is expressible.
    printf '# body\n'    > "$UPSTREAM/src-tauri/src/body.md"
    printf '// ui\n'     > "$UPSTREAM/src/app.ts"
    printf '[workspace]\n' > "$UPSTREAM/Cargo.toml"
    printf '# lock\n'      > "$UPSTREAM/Cargo.lock"
    # NOT `clippy.toml` / `rust-toolchain.toml` in the BASE fixture. Two arms
    # below run a REAL `cargo metadata`, and a `rust-toolchain.toml` carrying an
    # empty `[toolchain]` table makes RUSTUP refuse before cargo starts — which
    # turns the workspace-preflight arms' typed decline into an unclassified
    # failure. The arms that test those two files create them themselves.
    cat > "$UPSTREAM/src-tauri/Cargo.toml" <<'TOML'
[package]
name = "app"
[dependencies]
thing = { path = "../crates/thing" }
TOML
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
printf '%s | CARGO_TARGET_DIR=%s\n' "$*" "${CARGO_TARGET_DIR:-}" >> "$CARGO_SHIM_LOG"
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
#
# `</dev/null` is load-bearing, not tidiness. The hook now reads git's pre-push
# ref list from stdin (`push_pushed_refs`), so whatever stdin the test runner
# happens to hand this script would otherwise decide the range under test —
# a terminal, an idle pipe, or a CI harness's own input. Pinning it to an empty
# stdin is what makes these cases assert the HEAD fallback specifically; the
# stdin arms below feed it deliberately instead.
run_prepush() {
    HOOK_OUT="$(cd "$WORK" && CARGO_SHIM_LOG="$SHIM_LOG" PATH="$SHIM_BIN:$PATH" \
        bash "$PREPUSH" 2>&1 </dev/null)"
    HOOK_RC=$?
}

# The same, but feeding the hook a pre-push ref list on stdin, the way git does.
# $1 is the whole stdin text.
run_prepush_with_stdin() {
    HOOK_OUT="$(cd "$WORK" && CARGO_SHIM_LOG="$SHIM_LOG" PATH="$SHIM_BIN:$PATH" \
        bash "$PREPUSH" 2>&1 <<<"$1")"
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

echo "  -- content-only markdown under src-tauri/ cannot change the gate's verdict --"

# THE case for plan 2026-09-15-runner-prepush-cargo-gate-fires-on-markdown-only-
# diffs-and-builds-a-cold-per-worktree-target: a bundled command/skill body
# edited in place. fmt and clippy cannot judge it, so paying a whole-workspace
# build for it only trains sessions to skip the gate.
fixture
commit_change "src-tauri/src/body.md" "more prose"
run_prepush
# The exit code alone cannot tell a skip from a run (the shim exits 0 either
# way); "cargo never invoked" is the assertion that decides this case.
check "modified .md only -> hook exits 0" "0" "$HOOK_RC"
check "modified .md only -> cargo never invoked" "no" "$(cargo_ran)"
case "$HOOK_OUT" in
    *"only content edits to src-tauri/ markdown"*"src-tauri/src/body.md"*)
        pass_note "and it names the markdown paths it excused" ;;
    *) fail_note "expected the excused .md paths to be listed, got: $HOOK_OUT" ;;
esac

# ADDING one can break the build (a new include_str! pointing at it lands in the
# same push), so it still runs.
fixture
commit_change "src-tauri/src/new-skill.md" "# new"
run_prepush
check "added .md -> gate ATTEMPTED" "yes" "$(cargo_ran)"

# DELETING one is the compile error the filter exists to keep: include_str! of a
# missing path.
fixture
git -C "$WORK" rm --quiet src-tauri/src/body.md
git -C "$WORK" commit --quiet -m "delete body.md"
run_prepush
check "deleted .md -> gate ATTEMPTED" "yes" "$(cargo_ran)"

fixture
git -C "$WORK" mv src-tauri/src/body.md src-tauri/src/renamed.md
git -C "$WORK" commit --quiet -m "rename body.md"
run_prepush
check "renamed .md -> gate ATTEMPTED" "yes" "$(cargo_ran)"

# `include_str!` refuses a body that is not valid UTF-8, so an edit that
# re-encodes one (an editor saving Windows-1252) is NOT a harmless content edit.
fixture
printf '\xe9t\xe9\n' >> "$WORK/src-tauri/src/body.md"
git -C "$WORK" add -A >/dev/null
git -C "$WORK" commit --quiet -m "re-encode body.md as latin-1"
run_prepush
check "modified .md that is no longer valid UTF-8 -> gate ATTEMPTED" "yes" "$(cargo_ran)"

# A non-ASCII path is quoted by git unless -z is used, and a quoted path never
# ends in .md — so without -z this edit would run the gate for nothing.
fixture
# `$'...'` so the name holds the real UTF-8 bytes of "café". Inside double
# quotes `\xc3` stays a literal backslash, which is a path separator on Git Bash.
CAFE="$WORK/src-tauri/src/"$'caf\xc3\xa9.md'
printf '# cafe\n' > "$CAFE"
git -C "$WORK" add -A >/dev/null
git -C "$WORK" commit --quiet -m "add a non-ascii body"
git -C "$WORK" push --quiet origin HEAD:refs/heads/with-cafe >/dev/null 2>&1
git -C "$WORK" update-ref refs/remotes/origin/main HEAD
printf 'more\n' >> "$CAFE"
git -C "$WORK" add -A >/dev/null
git -C "$WORK" commit --quiet -m "edit the non-ascii body"
# Guard against a vacuous pass: the range must actually contain the edit.
check "non-ASCII fixture -> the pushed range holds exactly the edited path" \
    "src-tauri/src/"$'caf\xc3\xa9.md' \
    "$(git -C "$WORK" -c core.quotepath=false diff --name-only origin/main..HEAD)"
run_prepush
check "modified non-ASCII .md path -> cargo never invoked" "no" "$(cargo_ran)"

# A markdown edit must not excuse the Rust change riding beside it.
fixture
commit_change "src-tauri/src/body.md" "more prose"
commit_change "src-tauri/src/lib.rs" "// rust"
run_prepush
check "modified .md + modified .rs -> gate ATTEMPTED" "yes" "$(cargo_ran)"

echo "  -- a linked worktree borrows the shared target instead of building cold --"

# A linked worktree of the fixture, plus a stub cargo-guard.sh that answers the
# resolve-only question the way the real one does. The stub records that it was
# asked, so "did the hook consult the guard?" is observable.
worktree_fixture() {
    fixture
    WT="$(dirname "$WORK")/wt"
    git -C "$WORK" worktree add --quiet -b wt-branch "$WT" >/dev/null 2>&1
    git -C "$WT" config user.email t@example.com
    git -C "$WT" config user.name t
    printf '// wt rust change\n' >> "$WT/src-tauri/src/lib.rs"
    git -C "$WT" add -A >/dev/null
    git -C "$WT" commit --quiet -m "rust change in a linked worktree"
    SHARED_TARGET="$(dirname "$WORK")/shared-target"
    GUARD_LOG="$(dirname "$WORK")/guard-invocations"
    STUB_GUARD="$(dirname "$WORK")/cargo-guard-stub.sh"
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

run_prepush_in_wt() {
    HOOK_OUT="$(cd "$WT" && CARGO_SHIM_LOG="$SHIM_LOG" PATH="$SHIM_BIN:$PATH" "$@" \
        bash "$PREPUSH" 2>&1 </dev/null)"
    HOOK_RC=$?
}

# Every cargo call the hook made, reduced to the CARGO_TARGET_DIR it saw.
shim_targets() {
    sed -n 's/.* | CARGO_TARGET_DIR=//p' "$SHIM_LOG" | sort -u | tr '\n' ' ' | sed 's/ $//'
}

worktree_fixture
run_prepush_in_wt env QONTINUI_PREPUSH_CARGO_GUARD="$STUB_GUARD"
check "linked worktree + guard -> the gate still runs (exit 0)" "0" "$HOOK_RC"
check "linked worktree + guard -> the guard is asked in resolve-only mode" \
    "check RESOLVE_ONLY=1" "$(cat "$GUARD_LOG")"
# The metadata preflight runs before the resolution and needs no target, so it
# is excluded; fmt and clippy are what build.
check "linked worktree + guard -> fmt and clippy build into the shared target" \
    "$SHARED_TARGET" "$(grep -v '^metadata' "$SHIM_LOG" | sed -n 's/.* | CARGO_TARGET_DIR=//p' | sort -u)"
case "$HOOK_OUT" in
    *"reusing the shared target CARGO_TARGET_DIR=$SHARED_TARGET"*)
        pass_note "and it names the shared target it borrowed" ;;
    *) fail_note "expected the borrowed target to be named, got: $HOOK_OUT" ;;
esac

worktree_fixture
run_prepush_in_wt env QONTINUI_PREPUSH_CARGO_GUARD="$STUB_GUARD" CARGO_TARGET_DIR=/caller/chose/this
check "linked worktree + caller CARGO_TARGET_DIR -> the guard is NOT asked" "" "$(cat "$GUARD_LOG")"
check "linked worktree + caller CARGO_TARGET_DIR -> the caller's value is used" \
    "/caller/chose/this" "$(shim_targets)"

worktree_fixture
run_prepush_in_wt env QONTINUI_PREPUSH_CARGO_GUARD="$STUB_GUARD" STUB_GUARD_EXIT=2
check "linked worktree + guard refuses -> the gate still runs, never fails the push" "0" "$HOOK_RC"
check "linked worktree + guard refuses -> cargo still ran" "yes" "$(cargo_ran)"
check "linked worktree + guard refuses -> no CARGO_TARGET_DIR is invented" "" "$(shim_targets)"
case "$HOOK_OUT" in
    *"could not resolve a target"*) pass_note "and it says the shared target could not be resolved" ;;
    *) fail_note "expected a one-line note about the unresolved target, got: $HOOK_OUT" ;;
esac

worktree_fixture
run_prepush_in_wt env QONTINUI_PREPUSH_CARGO_GUARD="$(dirname "$WORK")/no-such-guard.sh"
check "linked worktree + no guard anywhere -> cargo still ran" "yes" "$(cargo_ran)"
check "linked worktree + no guard anywhere -> today's behaviour (no CARGO_TARGET_DIR)" "" "$(shim_targets)"
case "$HOOK_OUT" in
    *"no cargo-guard.sh to resolve the shared target"*) pass_note "and it says no guard was found" ;;
    *) fail_note "expected a note that no guard was found, got: $HOOK_OUT" ;;
esac

# Lookup candidate 3: no override and no QONTINUI_ROOT, so the guard is found
# beside the PRIMARY checkout, the way a workspace root lays repos out.
worktree_fixture
mkdir -p "$(dirname "$WORK")/qontinui-claude-config/scripts"
cp "$STUB_GUARD" "$(dirname "$WORK")/qontinui-claude-config/scripts/cargo-guard.sh"
run_prepush_in_wt env
check "linked worktree + guard beside the primary checkout -> it is found and asked" \
    "check RESOLVE_ONLY=1" "$(cat "$GUARD_LOG")"
check "linked worktree + guard beside the primary checkout -> fmt and clippy use its target" \
    "$SHARED_TARGET" "$(grep -v '^metadata' "$SHIM_LOG" | sed -n 's/.* | CARGO_TARGET_DIR=//p' | sort -u)"

# Lookup candidate 2: QONTINUI_ROOT names the workspace root.
worktree_fixture
mkdir -p "$(dirname "$WORK")/elsewhere/qontinui-claude-config/scripts"
cp "$STUB_GUARD" "$(dirname "$WORK")/elsewhere/qontinui-claude-config/scripts/cargo-guard.sh"
run_prepush_in_wt env QONTINUI_ROOT="$(dirname "$WORK")/elsewhere"
check "linked worktree + guard under QONTINUI_ROOT -> it is found and asked" \
    "check RESOLVE_ONLY=1" "$(cat "$GUARD_LOG")"

# A guard that answers 0 but names no target must not invent one.
worktree_fixture
printf '#!/usr/bin/env bash\nprintf "RUNNER_ROOT=%%s\\n" "$PWD"\n' > "$STUB_GUARD"
run_prepush_in_wt env QONTINUI_PREPUSH_CARGO_GUARD="$STUB_GUARD"
check "linked worktree + guard prints no TARGET_DIR -> cargo still ran" "yes" "$(cargo_ran)"
check "linked worktree + guard prints no TARGET_DIR -> no CARGO_TARGET_DIR" "" "$(shim_targets)"
case "$HOOK_OUT" in
    *"printed no TARGET_DIR"*) pass_note "and it says the guard named no target" ;;
    *) fail_note "expected a note that no TARGET_DIR was printed, got: $HOOK_OUT" ;;
esac

# Through a REAL `git push`, so git's own hook environment (GIT_DIR pointing at
# <primary>/.git/worktrees/<name>) is what the worktree detection sees. Every
# other case calls the hook directly with that environment unset.
worktree_fixture
HOOKS="$(dirname "$WORK")/hooks"
mkdir -p "$HOOKS"
cat > "$HOOKS/pre-push" <<HOOK
#!/usr/bin/env bash
exec bash "$PREPUSH"
HOOK
chmod +x "$HOOKS/pre-push"
git -C "$WORK" config core.hooksPath "$HOOKS"
HOOK_OUT="$(cd "$WT" && CARGO_SHIM_LOG="$SHIM_LOG" PATH="$SHIM_BIN:$PATH" \
    QONTINUI_PREPUSH_CARGO_GUARD="$STUB_GUARD" git push origin wt-branch 2>&1)"
HOOK_RC=$?
check "real git push from a linked worktree -> the push succeeds" "0" "$HOOK_RC"
check "real git push from a linked worktree -> fmt and clippy use the shared target" \
    "$SHARED_TARGET" "$(grep -v '^metadata' "$SHIM_LOG" | sed -n 's/.* | CARGO_TARGET_DIR=//p' | sort -u)"

# The primary checkout already builds into its own warm target; leave it alone.
worktree_fixture
: > "$SHIM_LOG"
HOOK_OUT="$(cd "$WORK" && commit_change "src-tauri/src/lib.rs" "// primary rust" && \
    CARGO_SHIM_LOG="$SHIM_LOG" PATH="$SHIM_BIN:$PATH" QONTINUI_PREPUSH_CARGO_GUARD="$STUB_GUARD" \
    bash "$PREPUSH" 2>&1)"
check "primary checkout -> cargo ran" "yes" "$(cargo_ran)"
check "primary checkout -> the guard is NOT asked" "" "$(cat "$GUARD_LOG")"
check "primary checkout -> no CARGO_TARGET_DIR is set" "" "$(shim_targets)"

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

echo "  -- the pushed range is what git says it is, not whatever HEAD points at --"

# THE regression this section exists for. `main` is checked out; a DIFFERENT
# branch carrying a Rust change is pushed. Scoped against HEAD the range is
# empty, so the gate reports a clean skip and an unlinted commit goes out —
# reproduced end-to-end on 2026-09-17 before the fix.
#
# `commit_change` commits on the current branch, so these build the feature
# branch and then return to main.
feature_branch_fixture() {
    fixture
    git -C "$WORK" checkout --quiet -b feature
    commit_change "$1" "$2"
    FEATURE_SHA="$(git -C "$WORK" rev-parse feature)"
    git -C "$WORK" checkout --quiet main
    # Guard against a vacuous pass: main really must carry none of it.
    MAIN_RANGE="$(git -C "$WORK" diff --name-only origin/main..main -- src-tauri/)"
}

# git's own protocol line: <local ref> <local sha> <remote ref> <remote sha>.
pushed_line() { printf '%s %s %s %s\n' "refs/heads/$1" "$2" "refs/heads/$1" "${3:-$ZERO_SHA}"; }
ZERO_SHA="0000000000000000000000000000000000000000"

feature_branch_fixture "src-tauri/src/lib.rs" "// rust on the branch being pushed"
check "fixture is honest -> HEAD (main) carries no src-tauri/ change" "" "$MAIN_RANGE"
run_prepush_with_stdin "$(pushed_line feature "$FEATURE_SHA")"
check "stdin names a non-checked-out branch with Rust -> gate ATTEMPTED" "yes" "$(cargo_ran)"

# The inverse, which is what proves the range came from stdin rather than from
# HEAD: the PUSHED branch is TS-only while HEAD carries Rust. A fix that merely
# widened the scope would run the gate here.
fixture
git -C "$WORK" checkout --quiet -b ts-only
commit_change "src/app.ts" "// ui only"
TS_SHA="$(git -C "$WORK" rev-parse ts-only)"
git -C "$WORK" checkout --quiet main
commit_change "src-tauri/src/lib.rs" "// rust that is NOT being pushed"
run_prepush_with_stdin "$(pushed_line ts-only "$TS_SHA")"
check "stdin names a TS-only branch while HEAD carries Rust -> gate SKIPPED" "no" "$(cargo_ran)"
check "and the hook exits 0" "0" "$HOOK_RC"

# pre-commit consumes git's stdin and re-exposes it as these two. Same answer.
feature_branch_fixture "src-tauri/src/lib.rs" "// rust on the branch being pushed"
HOOK_OUT="$(cd "$WORK" && CARGO_SHIM_LOG="$SHIM_LOG" PATH="$SHIM_BIN:$PATH" \
    PRE_COMMIT_TO_REF="$FEATURE_SHA" PRE_COMMIT_FROM_REF="$(git -C "$WORK" rev-parse origin/main)" \
    bash "$PREPUSH" 2>&1 </dev/null)"
check "PRE_COMMIT_TO_REF names the pushed branch -> gate ATTEMPTED" "yes" "$(cargo_ran)"

# The remote already has the ref: its sha is exactly the base, so only the new
# commits count. Here the Rust change is ALREADY on the remote and the pushed
# commit is TS-only — a HEAD-and-merge-base scope would re-gate the old commit.
fixture
commit_change "src-tauri/src/lib.rs" "// rust already pushed"
git -C "$WORK" push --quiet origin main >/dev/null 2>&1
REMOTE_SHA="$(git -C "$WORK" rev-parse HEAD)"
commit_change "src/app.ts" "// new ui commit"
run_prepush_with_stdin "$(pushed_line main "$(git -C "$WORK" rev-parse HEAD)" "$REMOTE_SHA")"
check "a non-zero remote sha is the base -> already-pushed Rust does not re-gate" "no" "$(cargo_ran)"

# A multi-ref push (`git push --all`): the verdict is over the UNION, so one
# Rust-bearing ref gates the whole push.
fixture
git -C "$WORK" checkout --quiet -b ts-branch
commit_change "src/app.ts" "// ui"
TS_SHA="$(git -C "$WORK" rev-parse HEAD)"
git -C "$WORK" checkout --quiet -b rust-branch
commit_change "src-tauri/src/lib.rs" "// rust"
RUST_SHA="$(git -C "$WORK" rev-parse HEAD)"
git -C "$WORK" checkout --quiet main
run_prepush_with_stdin "$(pushed_line ts-branch "$TS_SHA")
$(pushed_line rust-branch "$RUST_SHA")"
check "multi-ref push, one ref carries Rust -> gate ATTEMPTED" "yes" "$(cargo_ran)"

# A deletion carries no commits, so it must be DISCARDED — leaving no pushed
# ref information at all, which sends the hook to the HEAD fallback.
#
# HEAD here is deliberately TS-only, which is what makes this case tell the two
# behaviours apart. Keeping the all-zero line instead would put an unresolvable
# sha into the range list, the whole resolution would report incomplete, and
# the hook would fail OPEN and run the gate. Both readings are safe, so an
# arm whose HEAD carried Rust would pass either way and assert nothing.
fixture
commit_change "src/app.ts" "// ui only"
run_prepush_with_stdin "$(printf 'refs/heads/gone %s refs/heads/gone %s\n' "$ZERO_SHA" "$ZERO_SHA")"
check "a deletion-only push is discarded and falls back to HEAD -> gate SKIPPED" \
    "no" "$(cargo_ran)"

# ...and a deletion riding ALONGSIDE a real ref must not suppress that ref.
fixture
git -C "$WORK" checkout --quiet -b rust-branch
commit_change "src-tauri/src/lib.rs" "// rust"
RUST_SHA="$(git -C "$WORK" rev-parse HEAD)"
git -C "$WORK" checkout --quiet main
run_prepush_with_stdin "$(printf 'refs/heads/gone %s refs/heads/gone %s\n' "$ZERO_SHA" "$ZERO_SHA")
$(pushed_line rust-branch "$RUST_SHA")"
check "a deletion beside a real ref -> the real ref still decides -> gate ATTEMPTED" \
    "yes" "$(cargo_ran)"

# Anything that is not a ref list is not GUESSED at — but neither is it
# silently discarded. HEAD here is TS-only, so a hook that fell back to HEAD
# would skip; the gate running is what proves the unreadable input was treated
# as "I cannot see the whole push" rather than as "there is no push info".
fixture
commit_change "src/app.ts" "// ui"
run_prepush_with_stdin "this is not a ref list
neither is this one"
check "wholly unreadable stdin -> gate RUNS (fail open), not a HEAD fallback" \
    "yes" "$(cargo_ran)"

# THE case the round-2 review reproduced. One well-formed ref beside one
# malformed one: the surviving line is a strict SUBSET of the push, so scoping
# on it is a false skip. Here the dropped line is the Rust-bearing ref and the
# survivor is TS-only — the shape that made the old code print a clean skip.
fixture
git -C "$WORK" checkout --quiet -b rusty
commit_change "src-tauri/src/lib.rs" "// rust on the dropped ref"
RUSTY_SHA="$(git -C "$WORK" rev-parse HEAD)"
git -C "$WORK" checkout --quiet main
commit_change "src/app.ts" "// ui on the surviving ref"
run_prepush_with_stdin "refs/heads/rusty $RUSTY_SHA refs/heads/rusty
$(pushed_line main "$(git -C "$WORK" rev-parse HEAD)")"
check "a malformed ref beside a good one -> gate RUNS, never scoped to the survivor" \
    "yes" "$(cargo_ran)"

# End to end through git itself, pushing by SHA to a ref name that is not the
# checked-out branch — the `git push origin <sha>:refs/heads/x` spelling the
# plan named. git supplies stdin; nothing here fabricates it.
feature_branch_fixture "src-tauri/src/lib.rs" "// rust pushed by sha"
HOOKS="$(dirname "$WORK")/sha-push-hooks"
mkdir -p "$HOOKS"
cat > "$HOOKS/pre-push" <<HOOK
#!/usr/bin/env bash
exec bash "$PREPUSH"
HOOK
chmod +x "$HOOKS/pre-push"
git -C "$WORK" config core.hooksPath "$HOOKS"
HOOK_OUT="$(cd "$WORK" && CARGO_SHIM_LOG="$SHIM_LOG" PATH="$SHIM_BIN:$PATH" \
    git push origin "$FEATURE_SHA":refs/heads/topic 2>&1)"
HOOK_RC=$?
check "real 'git push origin <sha>:refs/heads/topic' -> the push succeeds" "0" "$HOOK_RC"
check "real 'git push origin <sha>:refs/heads/topic' -> gate ATTEMPTED" "yes" "$(cargo_ran)"

# The UTF-8 probe has to read the blob at the PUSHED TIP. Here the pushed branch
# MODIFIES a markdown body that HEAD (main) has deleted, so `HEAD:<path>` does
# not resolve at all — and a `cat-file` that fails reads as "not valid UTF-8"
# and gates for a reason that has nothing to do with the push.
fixture
git -C "$WORK" checkout --quiet -b mdbranch
commit_change "src-tauri/src/body.md" "more prose"
MD_SHA="$(git -C "$WORK" rev-parse HEAD)"
git -C "$WORK" checkout --quiet main
git -C "$WORK" rm --quiet src-tauri/src/body.md
git -C "$WORK" commit --quiet -m "delete body.md on main"
check "fixture is honest -> HEAD does not carry the path" \
    "" "$(git -C "$WORK" cat-file blob HEAD:src-tauri/src/body.md 2>/dev/null; echo -n '')"
run_prepush_with_stdin "$(pushed_line mdbranch "$MD_SHA")"
check "a modified .md is read at the pushed tip, not at HEAD -> gate SKIPPED" "no" "$(cargo_ran)"

# A ref list that STALLS mid-stream must read as incomplete, not as the whole
# push. If the dropped ref is the Rust-bearing one and a surviving ref is
# TS-only, a "complete" verdict over the union is a false skip.
#
# Two bugs hid here in a row, both of which LOOKED fixed: `$?` after `done` is
# the loop BODY's status, and `$?` inside `if ! read` is the NEGATION's. This
# arm is what distinguishes a real fix from either.
fixture
STALL_OUT="$(cd "$WORK" && bash -c '
    . "'"$SCRIPT_DIR"'/lib/push-range.sh"
    out="$(push_pushed_refs)"; rc=$?
    printf "rc=%s lines=%s" "$rc" "$(printf "%s\n" "$out" | grep -c .)"
  ' < <(printf "refs/heads/a %s refs/heads/a %s\n" "$(printf '1%.0s' $(seq 40))" "$ZERO_SHA"
        sleep 4
        printf "refs/heads/b %s refs/heads/b %s\n" "$(printf '2%.0s' $(seq 40))" "$ZERO_SHA"))"
check "a ref list that stalls mid-stream reads as INCOMPLETE" "rc=2 lines=1" "$STALL_OUT"

# ...and it has to reach the HOOK. The library returning a typed 2 is worth
# nothing if the caller maps it onto the same fallback as "no ref info at all":
# that converts "I could not read the whole push" into "measure HEAD instead",
# which is the exact false skip this file exists to abolish. HEAD is TS-only
# here, so a hook that fell back to HEAD would skip.
fixture
git -C "$WORK" checkout --quiet -b stalled-rust
commit_change "src-tauri/src/lib.rs" "// rust on the stalled ref"
STALLED_SHA="$(git -C "$WORK" rev-parse HEAD)"
git -C "$WORK" checkout --quiet main
commit_change "src/app.ts" "// ui on HEAD"
HOOK_OUT="$(cd "$WORK" && CARGO_SHIM_LOG="$SHIM_LOG" PATH="$SHIM_BIN:$PATH" \
    bash "$PREPUSH" 2>&1 < <(pushed_line stalled-rust "$STALLED_SHA"; sleep 4))"
check "a stalled ref list makes the HOOK run the gate, not fall back to HEAD" \
    "yes" "$(cargo_ran)"

# ...while a complete multi-ref list on the same code path is complete.
WHOLE_OUT="$(cd "$WORK" && bash -c '
    . "'"$SCRIPT_DIR"'/lib/push-range.sh"
    out="$(push_pushed_refs)"; rc=$?
    printf "rc=%s lines=%s" "$rc" "$(printf "%s\n" "$out" | grep -c .)"
  ' <<REFS
refs/heads/a $(printf '1%.0s' $(seq 40)) refs/heads/a $ZERO_SHA
refs/heads/b $(printf '2%.0s' $(seq 40)) refs/heads/b $ZERO_SHA
REFS
)"
check "a complete multi-ref list reads as COMPLETE" "rc=0 lines=2" "$WHOLE_OUT"

echo "  -- everything the gate COMPILES is in scope, not just src-tauri/ --"

# THE live false skip this section exists for. `cd src-tauri && cargo clippy`
# compiles the in-repo path dependencies, so a push touching only one of them
# breaks the gate's own build — and the old `-- src-tauri/` pathspec skipped it
# with "no src-tauri/ changes". `git log` holds a real instance:
# `2372b5de5 style(spec-check): apply cargo fmt to the crate`.
fixture
commit_change "crates/thing/src/lib.rs" "// changed the path dep"
run_prepush
check "a change to an in-repo path DEPENDENCY -> gate ATTEMPTED" "yes" "$(cargo_ran)"

# build.rs reads this file and is, in its own words, "deliberately FATAL on
# failure": a rename or a reshape is a hard build failure, not a lint.
fixture
git -C "$WORK" mv src/components/app/tab-types.ts src/components/app/tabs.ts
git -C "$WORK" commit --quiet -m "rename the build-script input"
run_prepush
check "renaming a build.rs input -> gate ATTEMPTED" "yes" "$(cargo_ran)"

# The build configuration decides what the gate's verdict even is: which
# clippy, which lint levels, which profile, which dependency versions.
for f in Cargo.toml Cargo.lock clippy.toml rust-toolchain.toml; do
    fixture
    commit_change "$f" "# touched"
    run_prepush
    check "a change to $f -> gate ATTEMPTED" "yes" "$(cargo_ran)"
done

# ...and the scope must still be the crate's INPUTS, not the whole repo: a
# TS-only push that touches none of them still skips. Without this the section
# above would be satisfied by simply gating everything.
fixture
commit_change "src/app.ts" "// ui only"
run_prepush
check "a TS change touching no gate input -> gate SKIPPED" "no" "$(cargo_ran)"

# A `.md` that is in scope because it sits inside a path-dep CRATE is not an
# embed, so the content excuse must not reach it. The excuse is keyed on the
# embed set; `*.md` alone would excuse any markdown the widened scope pulls in.
fixture
printf '# readme\n' > "$WORK/crates/thing/README.md"
git -C "$WORK" add -A >/dev/null
git -C "$WORK" commit --quiet -m "add a readme in the path dep"
git -C "$WORK" push --quiet origin HEAD:refs/heads/mdbase >/dev/null 2>&1
git -C "$WORK" update-ref refs/remotes/origin/main HEAD
commit_change "crates/thing/README.md" "more prose"
run_prepush
check "a MODIFIED .md inside a path-dep crate is not excused -> gate ATTEMPTED" \
    "yes" "$(cargo_ran)"

# ...while the same edit to a markdown body under src-tauri/ still is.
fixture
commit_change "src-tauri/src/body.md" "more prose"
run_prepush
check "a MODIFIED .md under src-tauri/ is still excused -> gate SKIPPED" \
    "no" "$(cargo_ran)"

# A path-dep .rs is a first-class Rust input, so a MODIFICATION gates even
# though a modified embed of the same status would be excused. The excuse is
# keyed on the embed set, not on "outside src-tauri/".
fixture
printf '// still valid utf-8\n' >> "$WORK/crates/thing/src/lib.rs"
git -C "$WORK" add -A >/dev/null
git -C "$WORK" commit --quiet -m "edit the path dep"
run_prepush
check "a MODIFIED path-dep source is not excused as content -> gate ATTEMPTED" \
    "yes" "$(cargo_ran)"

echo "  -- the skip switches still short-circuit everything above --"

# Both halves of each switch, because an opt-in tested only in its ON state
# says nothing about the arm that actually runs on every push.
fixture
commit_change "src-tauri/src/lib.rs" "// rust"
HOOK_OUT="$(cd "$WORK" && CARGO_SHIM_LOG="$SHIM_LOG" PATH="$SHIM_BIN:$PATH" \
    QONTINUI_PREPUSH_SKIP_ALL=1 bash "$PREPUSH" 2>&1 </dev/null)"
HOOK_RC=$?
check "QONTINUI_PREPUSH_SKIP_ALL=1 -> exit 0" "0" "$HOOK_RC"
check "QONTINUI_PREPUSH_SKIP_ALL=1 -> cargo never invoked" "no" "$(cargo_ran)"

fixture
commit_change "src-tauri/src/lib.rs" "// rust"
run_prepush
check "QONTINUI_PREPUSH_SKIP_ALL unset -> the same diff DOES run cargo" "yes" "$(cargo_ran)"

fixture
commit_change "src-tauri/src/lib.rs" "// rust"
HOOK_OUT="$(cd "$WORK" && CARGO_SHIM_LOG="$SHIM_LOG" PATH="$SHIM_BIN:$PATH" \
    QONTINUI_PREPUSH_SKIP=1 bash "$PREPUSH" 2>&1 </dev/null)"
HOOK_RC=$?
check "QONTINUI_PREPUSH_SKIP=1 -> exit 0" "0" "$HOOK_RC"
# The shim records each invocation's argv, so the two halves are separable.
# Spelled as ONE comparison of the actual subcommand list: an `&& echo x ||
# echo x` pair reads like an assertion and cannot fail.
check "QONTINUI_PREPUSH_SKIP=1 -> fmt ran and clippy did not" \
    "fmt" \
    "$(sed 's/ |.*//' "$SHIM_LOG" | grep -E '^(fmt|clippy)$' | sort -u | tr '\n' ' ' | sed 's/ $//')"
case "$HOOK_OUT" in
    *"skipping clippy (fmt above still ran)"*) pass_note "and it says clippy was the half skipped" ;;
    *) fail_note "expected the clippy-only skip note, got: $HOOK_OUT" ;;
esac

echo "  -- embedded files OUTSIDE src-tauri/ are in scope --"

# `include_str!` resolves relative to the .rs file that names it, so the crate
# compiles in files the `src-tauri/` pathspec cannot see. Give the fixture one.
embed_fixture() {
    fixture
    mkdir -p "$WORK/examples" "$WORK/specs/pages" "$WORK/docs"
    printf 'print("hi")\n' > "$WORK/examples/setup.py"
    printf '# page\n'       > "$WORK/specs/pages/home.md"
    printf '# unrelated\n'  > "$WORK/docs/not-embedded.md"
    cat > "$WORK/src-tauri/src/embed.rs" <<'RS'
const SETUP: &str = include_str!("../../examples/setup.py");
static PAGES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../specs/pages");
RS
    git -C "$WORK" add -A >/dev/null
    git -C "$WORK" commit --quiet -m "add outside-tree embeds"
    git -C "$WORK" push --quiet origin HEAD:refs/heads/embedbase >/dev/null 2>&1
    git -C "$WORK" update-ref refs/remotes/origin/main HEAD
}

embed_fixture
git -C "$WORK" rm --quiet examples/setup.py
git -C "$WORK" commit --quiet -m "delete an embedded file"
run_prepush
check "deleting an embedded file outside src-tauri/ -> gate ATTEMPTED" "yes" "$(cargo_ran)"

embed_fixture
git -C "$WORK" mv examples/setup.py examples/renamed.py
git -C "$WORK" commit --quiet -m "rename an embedded file"
run_prepush
check "renaming an embedded file outside src-tauri/ -> gate ATTEMPTED" "yes" "$(cargo_ran)"

# A content edit that stays valid UTF-8 cannot change what compiles, so it is
# excused for the same reason a modified .md under src-tauri/ is.
embed_fixture
commit_change "examples/setup.py" "print('more')"
run_prepush
check "editing an embedded file (valid UTF-8) -> gate SKIPPED" "no" "$(cargo_ran)"

# ...but re-encoding it is a compile error for include_str!, exactly as under
# src-tauri/.
embed_fixture
printf '\xe9t\xe9\n' >> "$WORK/examples/setup.py"
git -C "$WORK" add -A >/dev/null
git -C "$WORK" commit --quiet -m "re-encode the embedded file as latin-1"
run_prepush
check "re-encoding an embedded file to invalid UTF-8 -> gate ATTEMPTED" "yes" "$(cargo_ran)"

# The scope must widen to the EMBEDDED files, not to the whole repo. A deleted
# file nothing embeds still skips.
embed_fixture
git -C "$WORK" rm --quiet docs/not-embedded.md
git -C "$WORK" commit --quiet -m "delete a file nothing embeds"
run_prepush
check "deleting a NON-embedded file outside src-tauri/ -> gate SKIPPED" "no" "$(cargo_ran)"

# include_dir! embeds a TREE, so a new file appearing under it changes what
# compiles even though the directory path itself did not change.
embed_fixture
printf '# about\n' > "$WORK/specs/pages/about.md"
git -C "$WORK" add -A >/dev/null
git -C "$WORK" commit --quiet -m "add a page under the embedded directory"
run_prepush
check "adding a file under an include_dir! tree -> gate ATTEMPTED" "yes" "$(cargo_ran)"

# An embed spelling the deriver cannot read (a raw string) makes the input set
# UNKNOWN. UNKNOWN must run the gate, never skip it: deleting the file that
# raw-string embed names is a compile error the scoped diff cannot see.
embed_fixture
printf 'const RAW: &str = include_str!(r"../../docs/raw.md");\n' >> "$WORK/src-tauri/src/embed.rs"
printf '# raw\n' > "$WORK/docs/raw.md"
git -C "$WORK" add -A >/dev/null
git -C "$WORK" commit --quiet -m "add an embed spelling the deriver cannot read"
git -C "$WORK" update-ref refs/remotes/origin/main HEAD
git -C "$WORK" rm --quiet docs/raw.md
git -C "$WORK" commit --quiet -m "delete the raw-string embed target"
run_prepush
check "unreadable embed spelling (UNKNOWN input set) -> gate ATTEMPTED" "yes" "$(cargo_ran)"
case "$HOOK_OUT" in
    *"could not tell which files outside src-tauri/"*) pass_note "and it names the UNKNOWN as the reason" ;;
    *) fail_note "expected the UNKNOWN-input-set note, got: $HOOK_OUT" ;;
esac

echo
if [ "$SKIP" -gt 0 ]; then
    printf '%d passed, %d failed, %d SKIPPED (arms not exercised in this environment)\n' \
        "$PASS" "$FAIL" "$SKIP"
else
    printf '%d passed, %d failed\n' "$PASS" "$FAIL"
fi
[ "$FAIL" -eq 0 ]
