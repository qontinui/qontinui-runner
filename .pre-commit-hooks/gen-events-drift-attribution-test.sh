#!/usr/bin/env bash
#
# Tests for `lib/gen-events-attribution.sh` — the "is this drift THIS push's
# fault?" decision that `gen-events-drift.sh` splits its verdict on.
#
# Run: bash .pre-commit-hooks/gen-events-drift-attribution-test.sh
#
# Each case builds a throwaway git repo under $TMPDIR with a real "origin",
# so nothing here needs a Rust toolchain, a qontinui-schemas checkout, a
# network, or the ~minute release build the real hook pays. The library was
# split out of the hook precisely so this could be true: the expensive half
# (regenerate + compare) and the cheap half (attribute) have no reason to be
# exercised together, and a test that needed the expensive half would simply
# not get written.
#
# The property under test is asymmetric on purpose, and the cases pin BOTH
# directions:
#
#   * a push that touches a codegen input is never cleared   (no lost signal)
#   * a push that touches none of them is never blamed       (no false blame)
#   * a push whose attribution cannot be computed is never cleared (fail closed)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/gen-events-attribution.sh
. "$SCRIPT_DIR/lib/gen-events-attribution.sh"

# MUST come before the first `git`. Every fixture below is a throwaway repo
# addressed with `git -C "$WORK"` — and under a hook, GIT_DIR overrides `-C`, so
# without this the fixtures commit into the REAL repository. That is not
# hypothetical: it happened the first time this script ran from a pre-push hook.
gen_events_clear_inherited_git_env

PASS=0
FAIL=0
SKIP=0
WORK=""
UPSTREAM=""

# Every fixture root, removed on exit. `fixture` is called ~20 times and each
# call builds an upstream repo plus a clone, so without this a run leaves that
# many trees behind in $TMPDIR — on a dev box that is a slow leak, and on a
# CI runner it is disk the next job wanted. Same shape as the scratch cleanup
# in `gen-events-drift.sh`, which traps EXIT for exactly this reason.
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

pass_note() {
    printf '  ok   %s\n' "$1"
    PASS=$((PASS + 1))
}

fail_note() {
    printf '  FAIL %s\n' "$1"
    FAIL=$((FAIL + 1))
}

# A case this environment could not set up. Counted and reported in the summary
# rather than printed and forgotten: an arm that silently stops being exercised
# reads exactly like an arm that passes.
skip_note() {
    printf '  SKIP %s\n' "$1"
    SKIP=$((SKIP + 1))
}

# A repo with an `origin` remote whose `main` is the upstream of the local
# branch — the shape every real push has. A real clone rather than a faked
# remote-tracking ref, so `merge-base` resolves exactly as it does live.
fixture() {
    local root
    # Hard-fail rather than continue: without this a failed `mktemp` leaves
    # $WORK pointing at the PREVIOUS fixture, and every later case then
    # reports a plausible-looking verdict about the wrong repo.
    if ! root="$(mktemp -d -t gen-events-attr-XXXXXX)" || [ ! -d "$root" ]; then
        printf '  FATAL could not create a fixture directory\n' >&2
        exit 1
    fi
    FIXTURE_ROOTS+=("$root")
    UPSTREAM="$root/upstream"
    WORK="$root/work"

    git init --quiet --initial-branch=main "$UPSTREAM"
    git -C "$UPSTREAM" config user.email t@example.com
    git -C "$UPSTREAM" config user.name t
    # Fixtures are pure path bookkeeping — attribution never looks at file
    # CONTENT — so pin the line-ending translation off rather than let a
    # Windows checkout print a CRLF warning per file per case.
    git -C "$UPSTREAM" config core.autocrlf false
    # Fixture commits must not run the developer machine's git hooks. A global
    # `core.hooksPath` would otherwise aim these throwaway repos at this repo's
    # pre-commit install, and a hook failing on a two-line fixture would read
    # as an attribution bug.
    git -C "$UPSTREAM" config core.hooksPath "$UPSTREAM/.git/no-hooks"
    mkdir -p "$UPSTREAM/src-tauri/src" "$UPSTREAM/src-tauri/scripts" "$UPSTREAM/src"
    printf '// base\n' > "$UPSTREAM/src-tauri/src/lib.rs"
    printf '# gen\n'   > "$UPSTREAM/src-tauri/scripts/generate_types.sh"
    printf '// ui\n'   > "$UPSTREAM/src/app.ts"
    printf '[package]\n' > "$UPSTREAM/Cargo.toml"
    printf '# lock\n'    > "$UPSTREAM/Cargo.lock"
    git -C "$UPSTREAM" add -A >/dev/null
    git -C "$UPSTREAM" commit --quiet -m base

    # `--config` and not a post-clone `git config`: the setting has to be in
    # force for the CHECKOUT, or the tree lands with CRLF, every file then
    # reads as modified against the index, and every case reports "mine".
    git clone --quiet --config core.autocrlf=false "$UPSTREAM" "$WORK"
    git -C "$WORK" config user.email t@example.com
    git -C "$WORK" config user.name t
    git -C "$WORK" config core.hooksPath "$WORK/.git/no-hooks"

    # The fixture is only a fixture if git agrees. `GIT_DIR` and friends
    # OVERRIDE `git -C`, so a leaked one aims every command below at the
    # real repository — which is how a run from a pre-push hook once
    # committed fixtures onto the branch being pushed. `gen_events_clear_inherited_git_env`
    # is supposed to have prevented that; this is the assertion that it did,
    # and it fails LOUDLY rather than letting the run proceed against the
    # wrong repo. A future git that adds another such variable trips here.
    #
    # Compared through `cd ... && pwd -P` on BOTH sides rather than by string:
    # on Git Bash `$WORK` is an MSYS path (/tmp/...) while git answers with a
    # Windows one (C:/Users/...), and those name the same directory. Putting
    # both through the same shell builtin is what makes the check portable
    # rather than a guaranteed false positive on Windows.
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
}

# Append to a path in the work tree and commit it.
commit_change() {
    local path="$1" text="$2"
    mkdir -p "$WORK/$(dirname "$path")"
    printf '%s\n' "$text" >> "$WORK/$path"
    git -C "$WORK" add -A >/dev/null
    git -C "$WORK" commit --quiet -m "touch $path"
}

decide() {
    gen_events_attribution "$WORK"
}

echo "gen-events-drift attribution"
echo "  -- pre-existing: this push cannot have moved the bindings --"

fixture
commit_change "src/app.ts" "// more ui"
decide
check "a frontend-only commit is PRE-EXISTING" "pre-existing" "$ATTRIBUTION_STATE"

fixture
commit_change "README.md" "# readme"
decide
check "a docs-only commit is PRE-EXISTING" "pre-existing" "$ATTRIBUTION_STATE"

fixture
decide
check "no local commits at all is PRE-EXISTING" "pre-existing" "$ATTRIBUTION_STATE"

fixture
printf '// scratch\n' > "$WORK/src/scratch.ts"
decide
check "an untracked frontend file is PRE-EXISTING" "pre-existing" "$ATTRIBUTION_STATE"

echo "  -- mine: this push touches something that feeds schemas.json --"

fixture
commit_change "src-tauri/src/lib.rs" "// changed"
decide
check "a committed src-tauri/src change is MINE" "mine" "$ATTRIBUTION_STATE"

fixture
printf '// dirty\n' >> "$WORK/src-tauri/src/lib.rs"
decide
check "an UNSTAGED src-tauri/src change is MINE" "mine" "$ATTRIBUTION_STATE"

fixture
printf '// staged\n' >> "$WORK/src-tauri/src/lib.rs"
git -C "$WORK" add -A >/dev/null
decide
check "a STAGED src-tauri/src change is MINE" "mine" "$ATTRIBUTION_STATE"

# A new module is invisible to `git diff`, which is why the library also
# consults `ls-files --others`. Without that arm this case reads as innocent.
fixture
printf '// new module\n' > "$WORK/src-tauri/src/brand_new.rs"
decide
check "a brand-new UNTRACKED .rs file is MINE" "mine" "$ATTRIBUTION_STATE"

fixture
commit_change "Cargo.lock" "# bumped"
decide
check "a Cargo.lock bump is MINE (it pins schemars/serde)" "mine" "$ATTRIBUTION_STATE"

fixture
commit_change "Cargo.toml" "schemars = 1"
decide
check "a Cargo.toml change is MINE" "mine" "$ATTRIBUTION_STATE"

fixture
commit_change "src-tauri/scripts/generate_types.sh" "# tweak"
decide
check "the generator script itself is MINE" "mine" "$ATTRIBUTION_STATE"

echo "  -- the case the whole split exists for --"

# A PEER lands a schema change on main; I rebase onto it and push a
# frontend-only commit. At pre-push, pre-commit computes the changed-file set
# over the whole pushed RANGE, so the hook fires on the PEER's src-tauri
# change — and the pre-split hook told ME "your Rust changes would move
# ts/src/generated". Attribution must clear me.
fixture
printf '// peer schema change\n' >> "$UPSTREAM/src-tauri/src/lib.rs"
git -C "$UPSTREAM" add -A >/dev/null
git -C "$UPSTREAM" commit --quiet -m "peer: schema"
git -C "$WORK" fetch --quiet origin
git -C "$WORK" reset --hard --quiet origin/main
commit_change "src/app.ts" "// my ui change"
decide
check "a peer's schema commit in the pushed range is PRE-EXISTING" \
    "pre-existing" "$ATTRIBUTION_STATE"

# Same shape, but this time my own commit DOES touch Rust. The peer's commit
# must not dilute that: touching a codegen input is blaming enough on its own.
fixture
printf '// peer schema change\n' >> "$UPSTREAM/src-tauri/src/lib.rs"
git -C "$UPSTREAM" add -A >/dev/null
git -C "$UPSTREAM" commit --quiet -m "peer: schema"
git -C "$WORK" fetch --quiet origin
git -C "$WORK" reset --hard --quiet origin/main
commit_change "src-tauri/src/lib.rs" "// my rust change"
decide
check "my own Rust commit atop a peer's is still MINE" "mine" "$ATTRIBUTION_STATE"

# The codegen inputs are wider than the paths a diff has historically moved:
# `schemas.json` comes out of a release build of the whole crate graph, so an
# in-repo path dependency or the toolchain pin can move it without any
# `src-tauri/src` file changing. Pinned here because the asymmetry only works
# while the list stays complete — a narrowed list clears a guilty pusher.
for input in rust-toolchain.toml src-tauri/clorinde/src/lib.rs crates/spec-check/Cargo.toml; do
    fixture
    commit_change "$input" "# touched"
    decide
    check "$input is a codegen input, so it is MINE" "mine" "$ATTRIBUTION_STATE"
done

echo "  -- fail closed when the question cannot be answered --"

fixture
git -C "$WORK" branch --unset-upstream >/dev/null 2>&1
git -C "$WORK" remote remove origin >/dev/null 2>&1
git -C "$WORK" update-ref -d refs/remotes/origin/main >/dev/null 2>&1
git -C "$WORK" update-ref -d refs/remotes/origin/HEAD >/dev/null 2>&1
decide
check "no remote at all is UNAVAILABLE, not cleared" "unavailable" "$ATTRIBUTION_STATE"
if [ -n "${ATTRIBUTION_UNAVAILABLE_REASON:-}" ]; then
    pass_note "unavailable states a reason: $ATTRIBUTION_UNAVAILABLE_REASON"
else
    fail_note "unavailable must state a reason"
fi

if [ -n "${ATTRIBUTION_TOUCHED:-}" ]; then
    fail_note "unavailable must blame nothing, got: $ATTRIBUTION_TOUCHED"
else
    pass_note "the UNAVAILABLE arm blames nothing"
fi

# A repo with no commits at all. Reached in practice by a fresh `git init`
# before the first commit, and it must not read as 'nothing changed'.
fixture
EMPTY="$(dirname "$WORK")/empty"
git init --quiet --initial-branch=main "$EMPTY"
gen_events_attribution "$EMPTY"
check "a repo with no HEAD commit is UNAVAILABLE" "unavailable" "$ATTRIBUTION_STATE"

fixture
SHALLOW="$(dirname "$WORK")/shallow"
if CLONE_ERR="$(git clone --quiet --depth 1 --config core.autocrlf=false "file://$UPSTREAM" "$SHALLOW" 2>&1)"; then
    gen_events_attribution "$SHALLOW"
    check "a shallow clone is UNAVAILABLE, not cleared" "unavailable" "$ATTRIBUTION_STATE"
else
    skip_note "shallow-clone case: clone failed (${CLONE_ERR%%$'\n'*})"
fi

echo "  -- which ref 'before this push' is measured against --"

# Fallback 2: no upstream, but the remote publishes a default branch. Pointed
# at a non-`main` name so the resolved ref PROVES which fallback fired —
# with origin/HEAD -> origin/main the answer is the same string as fallback 3.
fixture
git -C "$WORK" update-ref refs/remotes/origin/trunk refs/remotes/origin/main
git -C "$WORK" symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/trunk
git -C "$WORK" update-ref -d refs/remotes/origin/main
git -C "$WORK" branch --unset-upstream >/dev/null 2>&1
commit_change "src/app.ts" "// ui"
decide
check "with no upstream, origin/HEAD is the base" "origin/trunk" "$ATTRIBUTION_BASE_REF"
check "and the verdict still holds" "pre-existing" "$ATTRIBUTION_STATE"

# Fallback 3: no upstream and no published default branch — the literal
# origin/main is the last resort before failing closed.
fixture
git -C "$WORK" branch --unset-upstream >/dev/null 2>&1
# `symbolic-ref --delete`, never `update-ref -d`: the latter DEREFERENCES, so it
# deletes refs/remotes/origin/main and leaves nothing for fallback 3 to find —
# which turns this case into a second copy of the no-remote one.
git -C "$WORK" symbolic-ref --delete refs/remotes/origin/HEAD >/dev/null 2>&1
commit_change "src-tauri/src/lib.rs" "// mine"
decide
check "with neither, origin/main is the base" "origin/main" "$ATTRIBUTION_BASE_REF"
check "and the verdict still holds" "mine" "$ATTRIBUTION_STATE"

echo "  -- a hook environment must not redirect the fixtures --"

# The defect this pins, verbatim: git exports GIT_DIR to every hook, GIT_DIR
# beats `git -C`, and the first real pre-push run of this script therefore
# committed its fixtures onto the branch being pushed and emptied the index.
# Recovered from the reflog; nothing reached origin only because the push then
# failed. `gen_events_clear_inherited_git_env` is the fix, and this is the test
# that would have caught it: point GIT_DIR at a DECOY repo, then assert the
# decision still measured the fixture and the decoy is untouched.
fixture
DECOY="$(dirname "$WORK")/decoy"
git init --quiet --initial-branch=main "$DECOY"
git -C "$DECOY" config user.email t@example.com
git -C "$DECOY" config user.name t
git -C "$DECOY" config core.hooksPath "$DECOY/.git/no-hooks"
git -C "$DECOY" commit --quiet --allow-empty -m "decoy tip"
DECOY_TIP_BEFORE="$(git -C "$DECOY" rev-parse HEAD)"
commit_change "src-tauri/src/lib.rs" "// mine"
(
    export GIT_DIR="$DECOY/.git"
    export GIT_WORK_TREE="$DECOY"
    gen_events_clear_inherited_git_env
    decide
    printf '%s\n' "$ATTRIBUTION_STATE" > "$WORK/.state"
) || true
check "a leaked GIT_DIR does not redirect the decision" "mine" "$(cat "$WORK/.state" 2>/dev/null)"
if [ "$(git -C "$DECOY" rev-parse HEAD)" = "$DECOY_TIP_BEFORE" ]; then
    pass_note "and the decoy repo was not written to"
else
    fail_note "the decoy repo was written to — a fixture escaped"
fi

echo "  -- what the failure message is allowed to claim --"

fixture
commit_change "src-tauri/src/lib.rs" "// changed"
decide
check "the MINE arm names the file it blamed" "src-tauri/src/lib.rs" "$ATTRIBUTION_TOUCHED"
if git -C "$WORK" rev-parse --verify --quiet "$ATTRIBUTION_BASE_SHA" >/dev/null 2>&1; then
    pass_note "base sha is a real commit (${ATTRIBUTION_BASE_SHA:0:12} via $ATTRIBUTION_BASE_REF)"
else
    fail_note "base sha is not a resolvable commit: $ATTRIBUTION_BASE_SHA"
fi

fixture
commit_change "src/app.ts" "// ui"
decide
check "the PRE-EXISTING arm blames nothing" "" "$ATTRIBUTION_TOUCHED"

echo
if [ "$SKIP" -gt 0 ]; then
    printf '%d passed, %d failed, %d SKIPPED (arms not exercised in this environment)\n' \
        "$PASS" "$FAIL" "$SKIP"
else
    printf '%d passed, %d failed\n' "$PASS" "$FAIL"
fi
[ "$FAIL" -eq 0 ]
