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
#
# Plus one section that reads the REAL tree rather than a fixture: the premise
# guard for the library's markdown exclusion, which must fail the day markdown
# can reach schemas.json.

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
    mkdir -p "$UPSTREAM/src-tauri/src/bin" "$UPSTREAM/src-tauri/scripts" "$UPSTREAM/src"
    printf '// base\n' > "$UPSTREAM/src-tauri/src/lib.rs"
    # The exporter, mentioning JsonSchema, beside the generator script: the
    # shape the premise guard's decision-time vacuity check expects. Without
    # it every fixture would read as an unverifiable premise.
    printf '// exports every schemars::JsonSchema type\n' > "$UPSTREAM/src-tauri/src/bin/export_schemas.rs"
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

# Put a path into the fixture's BASE (the upstream `main` the work clone is
# measured against), so a case can then edit it without the edit being the
# push's own commit.
seed_base() {
    local path="$1" text="$2"
    mkdir -p "$UPSTREAM/$(dirname "$path")"
    printf '%s\n' "$text" >> "$UPSTREAM/$path"
    git -C "$UPSTREAM" add -A >/dev/null
    git -C "$UPSTREAM" commit --quiet -m "base: $path"
    git -C "$WORK" fetch --quiet origin
    git -C "$WORK" reset --hard --quiet origin/main
}

# The ATTRIBUTION_TOUCHED_DETAIL line for one path — `<path><TAB><source>`.
detail_line() {
    printf '%s\t%s' "$1" "$2"
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

echo "  -- markdown under a codegen input directory feeds nothing (#1667) --"

# The push that motivated the exclusion: markdown only, under src-tauri/src —
# a directory-prefix input — so the pre-exclusion library blamed it.
fixture
commit_change "src-tauri/src/fleet_commands/x.md" "# a command body"
decide
check "a committed src-tauri/src/fleet_commands/x.md is PRE-EXISTING" \
    "pre-existing" "$ATTRIBUTION_STATE"

fixture
mkdir -p "$WORK/src-tauri/src/fleet_skills/new"
printf '# draft\n' > "$WORK/src-tauri/src/fleet_skills/new/SKILL.md"
decide
check "an UNTRACKED .md under src-tauri/src is PRE-EXISTING" "pre-existing" "$ATTRIBUTION_STATE"

fixture
seed_base "src-tauri/src/context/builtins/guide.md" "# guide"
printf '# edited\n' >> "$WORK/src-tauri/src/context/builtins/guide.md"
decide
check "an unstaged edit to a tracked .md under src-tauri/src is PRE-EXISTING" \
    "pre-existing" "$ATTRIBUTION_STATE"

echo "  -- mine: this push touches something that feeds schemas.json --"

fixture
commit_change "src-tauri/src/lib.rs" "// changed"
decide
check "a committed src-tauri/src change is MINE" "mine" "$ATTRIBUTION_STATE"

fixture
printf '// dirty\n' >> "$WORK/src-tauri/src/lib.rs"
decide
check "an UNSTAGED src-tauri/src change is MINE" "mine" "$ATTRIBUTION_STATE"
check "  and it is labelled uncommitted" \
    "$(detail_line src-tauri/src/lib.rs uncommitted)" "$ATTRIBUTION_TOUCHED_DETAIL"

fixture
printf '// staged\n' >> "$WORK/src-tauri/src/lib.rs"
git -C "$WORK" add -A >/dev/null
decide
check "a STAGED src-tauri/src change is MINE" "mine" "$ATTRIBUTION_STATE"
check "  and it is labelled uncommitted" \
    "$(detail_line src-tauri/src/lib.rs uncommitted)" "$ATTRIBUTION_TOUCHED_DETAIL"

# A new module is invisible to `git diff`, which is why the library also
# consults `ls-files --others`. Without that arm this case reads as innocent.
fixture
printf '// new module\n' > "$WORK/src-tauri/src/brand_new.rs"
decide
check "a brand-new UNTRACKED .rs file is MINE" "mine" "$ATTRIBUTION_STATE"
check "  and it is labelled untracked" \
    "$(detail_line src-tauri/src/brand_new.rs untracked)" "$ATTRIBUTION_TOUCHED_DETAIL"

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
check "  and its detail is empty too" "" "$ATTRIBUTION_TOUCHED_DETAIL"

# Markdown in the same commit as real Rust neither clears the Rust nor gets
# blamed beside it: the verdict is the Rust's, and so is the file list.
fixture
mkdir -p "$WORK/src-tauri/src/fleet_commands"
printf '// changed\n' >> "$WORK/src-tauri/src/lib.rs"
printf '# body\n' > "$WORK/src-tauri/src/fleet_commands/x.md"
git -C "$WORK" add -A >/dev/null
git -C "$WORK" commit --quiet -m "rust + markdown"
decide
check "a commit touching lib.rs AND an .md is MINE" "mine" "$ATTRIBUTION_STATE"
check "  and blames lib.rs alone" "src-tauri/src/lib.rs" "$ATTRIBUTION_TOUCHED"

# The #1667 shape in its other half: the committed input is in the push, the
# dirty lockfile is not — and the message must be able to say which is which.
# The verdict does not move (the regen reads the working tree); only the label.
fixture
commit_change "src-tauri/src/lib.rs" "// committed"
printf '# dirty\n' >> "$WORK/Cargo.lock"
decide
check "committed lib.rs + dirty Cargo.lock is still MINE" "mine" "$ATTRIBUTION_STATE"
check "  and the detail tells the two sources apart" \
    "$(detail_line Cargo.lock uncommitted)"$'\n'"$(detail_line src-tauri/src/lib.rs committed)" \
    "$ATTRIBUTION_TOUCHED_DETAIL"
check "  while the flat list is unchanged" \
    "Cargo.lock"$'\n'"src-tauri/src/lib.rs" "$ATTRIBUTION_TOUCHED"

# A path in two sources is listed once per source, not collapsed by precedence.
fixture
commit_change "src-tauri/src/lib.rs" "// committed"
printf '// and dirty\n' >> "$WORK/src-tauri/src/lib.rs"
decide
check "a committed AND dirty path is listed under both sources" \
    "$(detail_line src-tauri/src/lib.rs committed)"$'\n'"$(detail_line src-tauri/src/lib.rs uncommitted)" \
    "$ATTRIBUTION_TOUCHED_DETAIL"
check "  but once in the flat list" "src-tauri/src/lib.rs" "$ATTRIBUTION_TOUCHED"

echo "  -- premise guard: markdown still cannot reach schemas.json --"

# The markdown exclusion is a NARROWING of the input list, which the library
# header forbids except on evidence. The library re-checks that evidence on
# every decision (`gen_events_markdown_premise_violations`); this runs the SAME
# function against the REAL tree, so a violation is loud here too rather than
# only quietly widening the next pusher's list.
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd -P)"
REPO_TOP="$(git -C "$REPO_ROOT" rev-parse --show-toplevel 2>/dev/null || true)"
[ -n "$REPO_TOP" ] && REPO_TOP="$(cd "$REPO_TOP" && pwd -P)"
if [ "$REPO_TOP" != "$REPO_ROOT" ]; then
    skip_note "premise guard: $REPO_ROOT is not a git checkout, so the real tree cannot be read"
else
    VIOLATIONS="$(gen_events_markdown_premise_violations "$REPO_ROOT")" && PREMISE_RC=0 || PREMISE_RC=$?
    if [ "$PREMISE_RC" -eq 0 ] && [ -z "$VIOLATIONS" ]; then
        pass_note "no markdown in the real tree can reach schemas.json"
    else
        fail_note "markdown may now reach schemas.json (premise exit $PREMISE_RC):"
        printf '%s\n' "$VIOLATIONS" | sed 's/^/         /'
        printf '       Every attribution decision now drops the *.md exclusion and blames\n'
        printf '       markdown again. Restructure so no markdown can reach a JsonSchema\n'
        printf '       type, or delete the exclusion if that is no longer true by design.\n'
    fi
    # Non-vacuity on the real tree: the guard must actually be SEEING the
    # embedded markdown it reasons about. Zero would mean the query broke, not
    # that the premise holds. Counted as `.rs` paths only, so a multi-line
    # error message cannot inflate it.
    INCLUDED_MD="$(gen_events_premise_grep "$REPO_ROOT" -l -E 'include_(str|bytes)!\([^)]*\.md"' | grep -c '\.rs$' || true)"
    if [ "${INCLUDED_MD:-0}" -gt 0 ]; then
        pass_note "  and it saw the $INCLUDED_MD files that include_str! markdown"
    else
        fail_note "  premise guard saw no include_str!'d markdown at all — the query is broken"
    fi
    # Arms 1-2 intersect with the JsonSchema file list; an empty list would
    # make them unfalsifiable while still printing "no violation".
    SCHEMA_FILES="$(gen_events_premise_grep "$REPO_ROOT" -l -F 'JsonSchema' | grep -c '\.rs$' || true)"
    if [ "${SCHEMA_FILES:-0}" -gt 0 ]; then
        pass_note "  and it saw the $SCHEMA_FILES files that mention JsonSchema"
    else
        fail_note "  premise guard saw no JsonSchema file at all — arms 1-2 are neutered"
    fi
fi

# Non-vacuity on fixtures: each arm must fire on a tree that violates it, and
# the `let doc = include_str!` shape the real tree carries must not.
premise_fixture() {
    local root rel="${2:-src-tauri/src/m.rs}"
    if ! root="$(mktemp -d -t gen-events-premise-XXXXXX)" || [ ! -d "$root" ]; then
        printf '  FATAL could not create a fixture directory\n' >&2
        exit 1
    fi
    FIXTURE_ROOTS+=("$root")
    git init --quiet --initial-branch=main "$root"
    mkdir -p "$root/$(dirname "$rel")"
    printf '%s\n' "$1" > "$root/$rel"
    PREMISE_FIXTURE="$root"
}
premise_case() {
    local label="$1" want="$2" body="$3" rel="${4:-}" got rc
    premise_fixture "$body" $rel
    gen_events_markdown_premise_violations "$PREMISE_FIXTURE" >/dev/null && rc=0 || rc=$?
    case "$rc" in 0) got="clean" ;; 1) got="violation" ;; *) got="probe-failed($rc)" ;; esac
    check "$label" "$want" "$got"
}
premise_case "guard fires: include_str! .md beside JsonSchema" violation \
    '#[derive(JsonSchema)] struct S; const B: &str = include_str!("b.md");'
premise_case "guard fires: include_str!(concat!(.., \"/x.\", \"md\")) beside JsonSchema" violation \
    "$(printf '#[derive(JsonSchema)] struct S;\nconst B: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/x.", "md"));')"
premise_case "guard fires: include_bytes! .md beside JsonSchema" violation \
    '#[derive(JsonSchema)] struct S; const B: &[u8] = include_bytes!("b.md");'
premise_case "guard fires: include_dir! beside JsonSchema, no .md literal needed" violation \
    '#[derive(JsonSchema)] struct S; static D: Dir = include_dir!("$CARGO_MANIFEST_DIR/skills");'
premise_case "guard fires: #[doc = include_str!(..)]" violation \
    '#[doc = include_str!("b.md")] struct S;'
premise_case "guard fires: #[doc = concat!(.., include_str!(..))]" violation \
    '#[doc = concat!("Intro. ", include_str!("b.md"))] struct S;'
premise_case "guard fires: schemars(description = CONST)" violation \
    '#[schemars(description = BODY)] struct S;'
premise_case "guard fires: a rustfmt-wrapped schemars attribute with an embed" violation \
    "$(printf '#[schemars(\n    description = include_str!(\n        "b.md"\n    )\n)]\nstruct S;')"
premise_case "guard is quiet on let doc = include_str!(..) with no JsonSchema" clean \
    'fn t() { let doc = include_str!("b.md"); }'
premise_case "guard fires: schemars(example = CONST)" violation \
    '#[schemars(example = BODY)] struct S;'
premise_case "guard fires: schemars(extend(\"description\" = CONST))" violation \
    '#[schemars(extend("description" = BODY))] struct S;'
premise_case "guard is quiet on literal schemars values (string and number)" clean \
    '#[schemars(title = "S", range(min = 1))] struct S;'
premise_case "guard fires: include!(\"x.md\")" violation \
    'include!("body.md");'
premise_case "guard fires: #[path = \"x.md\"] mod" violation \
    '#[path = "body.md"] mod body;'
premise_case "guard fires: include!(concat!(..)) naming a .md" violation \
    "$(printf 'include!(concat!(\n    env!("CARGO_MANIFEST_DIR"),\n    "/body.md"\n));')"
premise_case "guard fires: include![\"x.md\"]" violation \
    'include!["body.md"];'
premise_case "guard fires: include!{\"x.md\"}" violation \
    'include!{"body.md"}'
premise_case "guard fires: include! (\"x.md\") with a space" violation \
    'include! ("body.md");'
premise_case "guard fires: #[cfg_attr(unix, path = \"x.md\")] mod" violation \
    '#[cfg_attr(unix, path = "body.md")] mod body;'
premise_case "guard fires: a rustfmt-wrapped cfg_attr path" violation \
    "$(printf '#[cfg_attr(\n    unix,\n    path = "body.md"\n)]\nmod body;')"
premise_case "guard is quiet on tracing's path = %p beside a .md literal" clean \
    'fn f() { debug!(path = %rel, "see notes.md"); }'
premise_case "guard is quiet on the real tree's include!(concat!(OUT_DIR, .rs)) and #[path = x.rs]" clean \
    "$(printf 'include!(concat!(env!("OUT_DIR"), "/valid_tab_ids.rs"));\n#[path = "../build.rs"]\nmod b;')"
premise_case "an exporter with no JsonSchema anywhere is UNVERIFIABLE, not clean" "probe-failed(2)" \
    'fn main() {}' src-tauri/src/bin/export_schemas.rs
premise_case "a generator script with no exporter beside it is UNVERIFIABLE" "probe-failed(2)" \
    '# gen' src-tauri/scripts/generate_types.sh
premise_case "guard fires: a build script reading markdown" violation \
    'fn main() { let b = std::fs::read_to_string("src/guide.md").unwrap(); }' src-tauri/build.rs
premise_case "guard fires: a build script filtering on the md extension" violation \
    'fn main() { if p.extension() == Some("md".as_ref()) {} }' src-tauri/build.rs
premise_case "guard is quiet on a build script that only mentions a .md in prose" clean \
    'fn main() { println!("See src-tauri/docs/tokio-console.md.\\n"); }' src-tauri/build.rs

echo "  -- the premise gates the exclusion at decision time --"

# The guard is not advisory: a tree that violates it gets NO markdown
# exclusion, so a markdown-only push is blamed again — the safe direction.
fixture
seed_base "src-tauri/src/schema.rs" '#[derive(JsonSchema)] struct S; const B: &str = include_str!("fleet_commands/x.md");'
commit_change "src-tauri/src/fleet_commands/x.md" "# a body a JsonSchema type embeds"
decide
check "with the premise violated, a committed .md is MINE" "mine" "$ATTRIBUTION_STATE"
case "$ATTRIBUTION_EXCLUDES_DROPPED_REASON" in
    "markdown may reach schemas.json ("*)
        pass_note "  and the decision says why: $ATTRIBUTION_EXCLUDES_DROPPED_REASON" ;;
    *)
        fail_note "  want a violation reason, got: '${ATTRIBUTION_EXCLUDES_DROPPED_REASON}'" ;;
esac

# Markdown compiled AS RUST, in both spellings: the exclusion must drop.
for AS_RUST in 'include!("fleet_commands/x.md");' '#[path = "fleet_commands/x.md"] mod body;'; do
    fixture
    seed_base "src-tauri/src/schema.rs" "$AS_RUST"
    commit_change "src-tauri/src/fleet_commands/x.md" "#[derive(JsonSchema)] struct S;"
    decide
    check "with markdown compiled as Rust ($AS_RUST), a committed .md is MINE" \
        "mine|yes" "$ATTRIBUTION_STATE|$(case "$ATTRIBUTION_EXCLUDES_DROPPED_REASON" in "markdown may reach"*) echo yes ;; *) echo no ;; esac)"
done

# A gitignored source still compiles, so the premise probe must read it.
fixture
seed_base ".gitignore" "src-tauri/src/gen.rs"
printf '#[derive(JsonSchema)] struct S;\nconst B: &str = include_str!("fleet_commands/x.md");\n' > "$WORK/src-tauri/src/gen.rs"
commit_change "src-tauri/src/fleet_commands/x.md" "# a body"
decide
check "a gitignored violating source is seen: the committed .md is MINE" \
    "mine|yes" "$ATTRIBUTION_STATE|$(case "$ATTRIBUTION_EXCLUDES_DROPPED_REASON" in *"src-tauri/src/gen.rs"*) echo yes ;; *) echo no ;; esac)"

fixture
commit_change "src-tauri/src/fleet_commands/x.md" "# a body"
decide
check "with the premise intact, the exclusion applies and no reason is set" \
    "pre-existing|" "$ATTRIBUTION_STATE|$ATTRIBUTION_EXCLUDES_DROPPED_REASON"

echo "  -- a git failure is UNAVAILABLE, never a cleared pusher --"

# A `git` on PATH that fails any invocation whose argument string matches a
# glob, and passes the rest to the real one. Before the fix,
# `$(git ... || true)` turned such a failure into an empty list, i.e.
# "touched nothing", i.e. PRE-EXISTING. One case per call, because each is its
# own way to lose the signal.
REAL_GIT="$(command -v git)"
git_shim_failing() {
    local name="$1" glob="$2" dir
    dir="$(dirname "$WORK")/shim-$name"
    mkdir -p "$dir"
    printf '#!/usr/bin/env bash\ncase "$*" in %s) echo "shim: %s fails" >&2; exit 128 ;; esac\nexec "%s" "$@"\n' \
        "$glob" "$name" "$REAL_GIT" > "$dir/git"
    chmod +x "$dir/git"
    printf '%s' "$dir"
}
shim_case() {
    local name="$1" glob="$2" want_reason="$3"
    fixture
    commit_change "src/app.ts" "// ui"
    SHIM="$(git_shim_failing "$name" "$glob")"
    ( PATH="$SHIM:$PATH"; decide; printf '%s|%s\n' "$ATTRIBUTION_STATE" "$ATTRIBUTION_UNAVAILABLE_REASON" > "$WORK/.state" )
    check "a failing $name is UNAVAILABLE, not PRE-EXISTING" \
        "unavailable" "$(cut -d'|' -f1 < "$WORK/.state")"
    check "  and the reason names the call" "yes" \
        "$(grep -qF -- "$want_reason" "$WORK/.state" && echo yes || echo no)"
}
shim_case "git diff base..HEAD" '*" diff --name-only --no-renames -z "[0-9a-f]*" HEAD -- "*' "HEAD failed, so this push's commits"
shim_case "git diff HEAD"       '*" diff --name-only --no-renames -z HEAD -- "*'          "git diff HEAD failed"
shim_case "git ls-files"        '*" ls-files "*'                              "ls-files --others failed"

# The premise probe failing is treated as a violation: exclusion dropped.
fixture
commit_change "src-tauri/src/fleet_commands/x.md" "# a body"
SHIM="$(git_shim_failing grep '*" grep "*')"
( PATH="$SHIM:$PATH"; decide; printf '%s|%s\n' "$ATTRIBUTION_STATE" "$ATTRIBUTION_EXCLUDES_DROPPED_REASON" > "$WORK/.state" )
check "a failing premise probe drops the exclusion, so the .md is MINE" \
    "mine" "$(cut -d'|' -f1 < "$WORK/.state")"
check "  and says the probe failed" "yes" \
    "$(grep -q 'probe failed' "$WORK/.state" && echo yes || echo no)"
check "  quoting the first ERROR line, with no fallback text appended" "yes|no" \
    "$(grep -qF '(ERROR git grep' "$WORK/.state" && echo yes || echo no)|$(grep -qF 'no ERROR line' "$WORK/.state" && echo yes || echo no)"

# A failing `comm` — the intersection behind arms 1-4 — must fail the probe,
# not leave those arms silently empty.
fixture
commit_change "src-tauri/src/fleet_commands/x.md" "# a body"
COMM_SHIM="$(dirname "$WORK")/shim-comm"
mkdir -p "$COMM_SHIM"
printf '#!/usr/bin/env bash\nexit 2\n' > "$COMM_SHIM/comm"
chmod +x "$COMM_SHIM/comm"
( PATH="$COMM_SHIM:$PATH"; decide; printf '%s|%s\n' "$ATTRIBUTION_STATE" "$ATTRIBUTION_EXCLUDES_DROPPED_REASON" > "$WORK/.state" )
check "a failing comm drops the exclusion, so the .md is MINE" \
    "mine" "$(cut -d'|' -f1 < "$WORK/.state")"
check "  and the reason names the failed intersection" yes \
    "$(grep -qF 'intersecting the premise lists failed' "$WORK/.state" && echo yes || echo no)"

echo "  -- the hook runs under set -e: the MINE path must survive it --"

# The hook is `set -euo pipefail`. A non-zero status leaking out of the
# decision or the renderer would abort it mid-message.
fixture
seed_base "src-tauri/src/schema.rs" '#[derive(JsonSchema)] struct S; const B: &str = include_str!("fleet_commands/x.md");'
commit_change "src-tauri/src/fleet_commands/x.md" "# a body"
( set -euo pipefail; decide; gen_events_render_mine >/dev/null ) && SETE_RC=0 || SETE_RC=$?
check "set -e: a violated-premise MINE decision and render exit 0" "0" "$SETE_RC"
fixture
commit_change "src-tauri/src/lib.rs" "// mine"
( set -euo pipefail; decide; gen_events_render_mine >/dev/null ) && SETE_RC=0 || SETE_RC=$?
check "set -e: a normal MINE decision and render exit 0" "0" "$SETE_RC"

echo "  -- the MINE message the hook prints --"

# `gen_events_render_mine` renders from the ATTRIBUTION_* variables alone, so
# these set them directly rather than building a repo per case.
render_with() {
    ATTRIBUTION_BASE_REF="origin/main"
    ATTRIBUTION_BASE_SHA="0123456789abcdef"
    ATTRIBUTION_EXCLUDES_DROPPED_REASON=""
    ATTRIBUTION_TOUCHED_DETAIL="$1"
    RENDERED="$(gen_events_render_mine)"
}
has() { printf '%s\n' "$RENDERED" | grep -qF -- "$1" && echo yes || echo no; }

LOCAL_NOTE="Inputs marked 'not part of this push' are local working-tree state"
LOCAL_REMEDY="set them aside so the working tree"

render_with "$(detail_line src-tauri/src/lib.rs committed)"
check "all committed: the lead line says this push" yes "$(has 'This push changes sources')"
check "  the file is labelled committed" yes "$(has 'src-tauri/src/lib.rs  (committed in this push)')"
check "  no working-tree note" no "$(has "$LOCAL_NOTE")"
check "  the pre-existing caveat is kept" yes "$(has 'Part of the diff may still be pre-existing')"

render_with "$(detail_line Cargo.lock uncommitted)"
check "uncommitted only: the lead line blames the working tree, not the push" \
    "yes|no" "$(has 'Your working tree (not this push' )|$(has 'This push changes')"
check "  the file is labelled as not in the push" \
    yes "$(has 'Cargo.lock  (uncommitted changes — not part of this push)')"
check "  the working-tree note is printed" yes "$(has "$LOCAL_NOTE")"
check "  and it says to set them aside, not to commit them" "yes|no" \
    "$(has "$LOCAL_REMEDY")|$(has 'commit or discard')"
check "  and the message prints no commands" no \
    "$(printf '%s\n' "$RENDERED" | grep -qE 'git (stash|checkout|worktree|reset)' && echo yes || echo no)"

render_with "$(detail_line src-tauri/src/new.rs untracked)"
check "untracked: labelled as not in the push" \
    yes "$(has 'src-tauri/src/new.rs  (untracked — not part of this push)')"
check "  and gets the working-tree note" yes "$(has "$LOCAL_NOTE")"

render_with "$(detail_line src-tauri/src/lib.rs committed)"$'\n'"$(detail_line src-tauri/src/lib.rs uncommitted)"
check "committed AND dirty: the push is still named in the lead line" yes "$(has 'This push changes sources')"
check "  both labels are printed" "yes|yes" \
    "$(has '(committed in this push)')|$(has '(uncommitted changes — not part of this push)')"
check "  and the dirty half gets the working-tree note" yes "$(has "$LOCAL_NOTE")"

render_with "$(detail_line src-tauri/src/lib.rs committed)"
ATTRIBUTION_EXCLUDES_DROPPED_REASON="markdown may reach schemas.json (x)"
RENDERED="$(gen_events_render_mine)"
check "a dropped exclusion is stated in one line" yes "$(has 'Markdown was counted as a codegen input this time')"

# Everything above pins the renderer; this pins that the hook still USES it,
# so the tests describe the message a pusher actually sees.
check "gen-events-drift.sh renders its MINE arm through gen_events_render_mine" yes \
    "$(grep -qE '^[[:space:]]*gen_events_render_mine[[:space:]]*\|' "$SCRIPT_DIR/gen-events-drift.sh" && echo yes || echo no)"

echo "  -- odd file names are shown as themselves --"

# git C-quotes non-ASCII, `"` and `\` without `-z`, and the label would then
# name a file that does not exist.
fixture
ODD_E='src-tauri/src/é b.rs'
ODD_Q='src-tauri/src/q"t.rs'
printf '// e\n' > "$WORK/$ODD_E"
printf '// q\n' > "$WORK/$ODD_Q"
ODD_T=""
if printf '// t\n' > "$WORK/src-tauri/src/a"$'\t'"b.rs" 2>/dev/null; then
    ODD_T="src-tauri/src/a"$'\t'"b.rs"
fi
decide
check "odd names are MINE" "mine" "$ATTRIBUTION_STATE"
check "  é b.rs is labelled by its real name" yes \
    "$(printf '%s\n' "$ATTRIBUTION_TOUCHED_DETAIL" | grep -qxF "$ODD_E"$'\t'"untracked" && echo yes || echo no)"
check "  q\"t.rs is labelled by its real name" yes \
    "$(printf '%s\n' "$ATTRIBUTION_TOUCHED_DETAIL" | grep -qxF "$ODD_Q"$'\t'"untracked" && echo yes || echo no)"
if [ -n "$ODD_T" ]; then
    ODD_T_SHOWN="$(printf '%q' "$ODD_T")"
    check "  a tab-named file is kept, in its %q form" yes \
        "$(printf '%s\n' "$ATTRIBUTION_TOUCHED_DETAIL" | grep -qxF "$ODD_T_SHOWN"$'\t'"untracked" && echo yes || echo no)"
    check "  and counted in the flat list" yes \
        "$(printf '%s\n' "$ATTRIBUTION_TOUCHED" | grep -qxF "$ODD_T_SHOWN" && echo yes || echo no)"
else
    skip_note "tab-named file: this filesystem refuses a tab in a file name"
fi
RENDERED="$(gen_events_render_mine)"
check "  the message shows é b.rs raw" yes \
    "$(printf '%s\n' "$RENDERED" | grep -qF "    $ODD_E  (untracked" && echo yes || echo no)"

echo "  -- renames and deletions: both halves are inputs --"

# The old path's disappearance moves the bindings as surely as the new path's
# arrival; with rename detection on, git would name only the new one.
fixture
seed_base "src-tauri/src/a.rs" "// a module"
git -C "$WORK" mv src-tauri/src/a.rs src-tauri/src/b.rs
git -C "$WORK" commit --quiet -m "rename a -> b"
decide
check "a committed rename lists both halves" \
    "src-tauri/src/a.rs"$'\n'"src-tauri/src/b.rs" "$ATTRIBUTION_TOUCHED"

fixture
seed_base "src-tauri/src/a.rs" "// a module"
git -C "$WORK" mv src-tauri/src/a.rs src-tauri/src/b.rs
decide
check "a staged rename lists both halves as uncommitted" \
    "$(detail_line src-tauri/src/a.rs uncommitted)"$'\n'"$(detail_line src-tauri/src/b.rs uncommitted)" \
    "$ATTRIBUTION_TOUCHED_DETAIL"

fixture
git -C "$WORK" rm --quiet src-tauri/src/lib.rs
decide
check "a staged git rm is listed as uncommitted" \
    "$(detail_line src-tauri/src/lib.rs uncommitted)" "$ATTRIBUTION_TOUCHED_DETAIL"

echo
if [ "$SKIP" -gt 0 ]; then
    printf '%d passed, %d failed, %d SKIPPED (arms not exercised in this environment)\n' \
        "$PASS" "$FAIL" "$SKIP"
else
    printf '%d passed, %d failed\n' "$PASS" "$FAIL"
fi
[ "$FAIL" -eq 0 ]
