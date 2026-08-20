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

PASS=0
FAIL=0
WORK=""
UPSTREAM=""

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

# A repo with an `origin` remote whose `main` is the upstream of the local
# branch — the shape every real push has. A real clone rather than a faked
# remote-tracking ref, so `merge-base` resolves exactly as it does live.
fixture() {
    local root
    root="$(mktemp -d -t gen-events-attr-XXXXXX)"
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

fixture
SHALLOW="$(dirname "$WORK")/shallow"
if git clone --quiet --depth 1 --config core.autocrlf=false "file://$UPSTREAM" "$SHALLOW" 2>/dev/null; then
    gen_events_attribution "$SHALLOW"
    check "a shallow clone is UNAVAILABLE, not cleared" "unavailable" "$ATTRIBUTION_STATE"
else
    printf '  skip shallow-clone case (clone unsupported in this environment)\n'
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
printf '%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
