#!/usr/bin/env bash
#
# Tests for `lib/crate-inputs.sh` — the derivation of "which files OUTSIDE
# src-tauri/ does the crate compile in?" that `cargo-prepush.sh` scopes on.
#
# Run: bash .pre-commit-hooks/crate-inputs-test.sh
#
# One test file per lib file, the convention `push-range-test.sh` states in its
# own header. The throwaway-git-repo harness is copied from there, which is the
# point of citing it as a pattern.
#
# THE REGRESSION THIS FILE EXISTS FOR
#
# The derivation only helps while it can READ the repo's macro spellings. The
# moment one invocation reports UNKNOWN, `cargo-prepush.sh` fails open on every
# push — correct, but it silently re-charges every TS-only and markdown-only
# push the full Rust gate, which is the cost qontinui-runner#1556 removed. That
# failure is invisible: the gate running looks exactly like the gate being
# needed. So the last section pins the REAL repository at zero unresolved
# invocations.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=lib/crate-inputs.sh
. "$SCRIPT_DIR/lib/crate-inputs.sh"

# MUST come before the first `git` — see push-range-test.sh's own note. Git
# exports GIT_DIR to every hook it runs and that OVERRIDES `git -C`, so without
# this a run from a real pre-push hook aims every fixture at the real repo.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
    GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR GIT_PREFIX \
    GIT_INTERNAL_SUPER_PREFIX GIT_CONFIG GIT_CONFIG_COUNT \
    GIT_CONFIG_GLOBAL GIT_CONFIG_SYSTEM GIT_NAMESPACE \
    GIT_INDEX_VERSION GIT_QUARANTINE_PATH GIT_PUSH_CERT \
    GIT_REFLOG_ACTION

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

# A repo whose only Rust file is whatever the caller passes on stdin. Sets
# $WORK and commits, so the derivation can be asked at a real rev.
rs_fixture() {
    local root
    if ! root="$(mktemp -d -t embedded-paths-XXXXXX)" || [ ! -d "$root" ]; then
        printf '  FATAL could not create a fixture directory\n' >&2
        exit 1
    fi
    FIXTURE_ROOTS+=("$root")
    WORK="$root/work"
    git init --quiet --initial-branch=main "$WORK"
    git -C "$WORK" config user.email t@example.com
    git -C "$WORK" config user.name t
    git -C "$WORK" config core.autocrlf false
    git -C "$WORK" config core.hooksPath "$WORK/.git/no-hooks"
    mkdir -p "$WORK/src-tauri/src"
    cat > "$WORK/src-tauri/src/embed.rs"
    git -C "$WORK" add -A >/dev/null
    git -C "$WORK" commit --quiet -m fixture
}

# Run the derivation over the fixture. Sets two globals rather than printing,
# because the RETURN CODE is half the answer — "no outside-tree embeds" and
# "could not tell" are different verdicts and a command substitution would
# throw the second one away.
DERIVE_RC=0
DERIVED=""
derive() {
    local out
    out="$(embedded_outside_paths "${1:-$WORK}" HEAD)"
    DERIVE_RC=$?
    DERIVED="$(printf '%s\n' "$out" | grep -v '^$' | sort | tr '\n' ' ' | sed 's/ $//')"
}

echo "embedded-paths derivation"
echo "  -- path resolution --"

rs_fixture <<'RS'
const A: &str = include_str!("../../examples/setup.py");
RS
derive
check "a relative literal resolves against the .rs file's own directory" \
    "examples/setup.py" "$DERIVED"
check "and the answer is complete" "0" "$DERIVE_RC"

rs_fixture <<'RS'
static P: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../specs/pages");
RS
derive
check "\$CARGO_MANIFEST_DIR resolves against src-tauri/" "specs/pages" "$DERIVED"
check "and the answer is complete" "0" "$DERIVE_RC"

# The crate-root idiom, spanning three lines. Read through the blob window.
rs_fixture <<'RS'
const S: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/mcp/unified_workflows.rs"
));
RS
derive
check "concat!(env!(CARGO_MANIFEST_DIR), ...) resolves INSIDE the tree, so nothing is emitted" \
    "" "$DERIVED"
check "and it is NOT reported unknown — this spelling is in the repo's own idiom" \
    "0" "$DERIVE_RC"

# A literal on the line after the macro: same window retry, different shape.
rs_fixture <<'RS'
const S: &str =
    include_str!(
        "../../docs/layers.md"
    );
RS
derive
check "a literal on a continuation line still resolves" "docs/layers.md" "$DERIVED"
check "and the answer is complete" "0" "$DERIVE_RC"

rs_fixture <<'RS'
const A: &str = include_str!("../schema.generated");
const B: &str = include_str!("../../src-tauri/other.txt");
RS
derive
check "literals resolving INSIDE src-tauri/ are not emitted" "" "$DERIVED"
check "and the answer is complete" "0" "$DERIVE_RC"

rs_fixture <<'RS'
const A: &str = include_str!(concat!(env!("OUT_DIR"), "/generated.rs"));
const B: &str = include_str!("$OUT_DIR/thing.rs");
RS
derive
check "a build-generated target is skipped, not emitted" "" "$DERIVED"
check "and a \$OUT_DIR literal is a SKIP, not an unknown" "0" "$DERIVE_RC"

rs_fixture <<'RS'
const A: &str = include_str!("../../a.txt");
const B: &str = include_str!("../../../escapes-the-repo.txt");
RS
derive
check "a literal escaping the repo root is dropped" "a.txt" "$DERIVED"
# The rc is half the answer: an escaping literal is a RESOLVED path that is not
# ours, not an unreadable one. Without this the arm passes identically against
# an implementation that reported UNKNOWN here and made the gate run always.
check "and it is a drop, not an unknown" "0" "$DERIVE_RC"

echo "  -- honest UNKNOWN, and what must NOT be one --"

# A spelling this reader cannot follow must say so, because the caller's only
# safe reading of an understated scope is to run the gate.
rs_fixture <<'RS'
const A: &str = include_str!(r"../../raw-string.txt");
RS
derive
check "a raw-string literal is reported UNKNOWN rather than silently dropped" \
    "1" "$DERIVE_RC"

# ...but prose naming the macro without calling it must not be. This is the
# case that would otherwise disarm the feature permanently: the library's own
# header and several source comments name `include_str!` in prose.
rs_fixture <<'RS'
// `include_str!` resolves it at compile time from this file's directory.
/// See include_dir! for the tree form.
const A: &str = include_str!("../../real.txt");
RS
derive
check "prose naming the macro is not mistaken for an invocation" "real.txt" "$DERIVED"
check "and does not read as unknown" "0" "$DERIVE_RC"

# THE MASKING CASE. One unreadable invocation beside a readable one must not be
# reported as a complete answer: the caller would take the scope-is-complete
# branch on an understated scope. Both orderings, because a `found`-style flag
# hides it in exactly one of them.
rs_fixture <<'RS'
const A: &str = include_str!("../../docs/a.md");
const B: &[u8] = include_bytes!(ASSET_PATH);
RS
derive
check "readable-then-unreadable on one line -> UNKNOWN" "1" "$DERIVE_RC"

rs_fixture <<'RS'
const B: &[u8] = include_bytes!(ASSET_PATH);
const A: &str = include_str!("../../docs/a.md");
RS
derive
check "unreadable-then-readable on one line -> UNKNOWN" "1" "$DERIVE_RC"

# ...and across the 5-line blob window, which is how ordinary formatting
# reaches this shape without two macros on one physical line.
rs_fixture <<'RS'
static DEMOS: &[&str] = &[
    include_str!(P),
    include_str!("../../examples/demo/b.py"),
];
RS
derive
check "an unreadable invocation inside the blob window -> UNKNOWN" "1" "$DERIVE_RC"

echo "  -- path dependencies, build.rs inputs and the build config --"

# `cd src-tauri && cargo clippy` compiles the in-repo path deps, so a push that
# touches only one of them must gate. This was the live false skip.
rs_fixture <<'RS'
// no embeds here
RS
mkdir -p "$WORK/crates/thing/src" "$WORK/src/components/app"
cat > "$WORK/src-tauri/Cargo.toml" <<'TOML'
[package]
name = "app"
[dependencies]
thing = { path = "../crates/thing" }
away = { path = "../../other-repo/rust" }
inner = { path = "./clorinde" }
[[bin]]
name = "x"
path = "src/bin/x.rs"
TOML
cat > "$WORK/src-tauri/build.rs" <<'RS2'
fn main() {
    const TAB_TYPES_TS: &str = "../src/components/app/tab-types.ts";
    println!("cargo:rerun-if-changed={TAB_TYPES_TS}");
}
RS2
git -C "$WORK" add -A >/dev/null
git -C "$WORK" commit --quiet -m "manifests"

# A workspace member NOTHING path-deps. Cargo loads every member manifest to
# build the workspace, so a syntax error in one fails the gate even though no
# crate depends on it — and the root `Cargo.toml` pathspec does not reach it:
# a git pathspec of `Cargo.toml` matches the ROOT file alone.
# A real path dep has a manifest of its own, so it is derived by BOTH passes.
# The set must still be deduplicated.
printf '[package]\nname = "thing"\n' > "$WORK/crates/thing/Cargo.toml"
mkdir -p "$WORK/crates/orphan/src"
printf '[package]\nname = "orphan"\n' > "$WORK/crates/orphan/Cargo.toml"
printf '// orphan\n' > "$WORK/crates/orphan/src/lib.rs"
printf '[workspace]\nmembers = ["src-tauri", "crates/thing", "crates/orphan"]\n' > "$WORK/Cargo.toml"
git -C "$WORK" add -A >/dev/null
git -C "$WORK" commit --quiet -m "add a member nothing depends on"

DEPS="$(crate_path_dep_dirs "$WORK" HEAD | sort -u | tr '
' ' ' | sed 's/ $//')"
check "a path dep AND a member nothing depends on are both derived" \
    "crates/orphan crates/thing" "$DEPS"
case "$DEPS" in
    *other-repo*) fail_note "a path dep escaping the repo must be dropped" ;;
    *) pass_note "a path dep escaping the repo is dropped" ;;
esac
case "$DEPS" in
    *clorinde*|*src/bin*) fail_note "paths inside src-tauri/ must not be emitted" ;;
    *) pass_note "a path inside src-tauri/ is not re-emitted" ;;
esac

BUILDIN="$(crate_build_script_inputs "$WORK" HEAD | sort | tr '
' ' ' | sed 's/ $//')"
check "what build.rs reads outside its dir is derived"     "src/components/app/tab-types.ts" "$BUILDIN"

CONF="$(crate_build_config_paths | tr '
' ' ')"
for want in Cargo.toml Cargo.lock rust-toolchain.toml clippy.toml rustfmt.toml .cargo; do
    case " $CONF " in
        *" $want "*) pass_note "build config covers $want" ;;
        *) fail_note "build config omits $want" ;;
    esac
done

# A repo with no build.rs is an EMPTY answer, never an unknown.
rs_fixture <<'RS'
// nothing
RS
crate_build_script_inputs "$WORK" HEAD >/dev/null
check "no build.rs -> empty, not unknown" "0" "$?"

echo "  -- normalization --"

for t in \
    "src-tauri/src/../../docs/x.md|docs/x.md" \
    "src-tauri/src/./a.rs|src-tauri/src/a.rs" \
    "a/b/../c|a/c" \
    "a/b/../../../escape|" \
    "a//b|a/b" \
    "src-tauri/x|src-tauri/x"
do
    in="${t%%|*}"; want="${t##*|}"
    check "normalize $in" "$want" "$(embedded_paths_normalize "$in")"
done

echo "  -- the REAL repository --"

# THE regression pin. See this file's header: one unreadable invocation makes
# `cargo-prepush.sh` fail open on every push, which is invisible because a gate
# that runs looks exactly like a gate that was needed.
real_out="$(crate_input_paths "$REPO_ROOT" HEAD)"
real_rc=$?
real_out="$(printf '%s\n' "$real_out" | grep -v '^$' | sort | tr '\n' ' ' | sed 's/ $//')"
check "every input source in this repo is fully readable (no UNKNOWN)" "0" "$real_rc"

# A derived path that is not in the tree means the resolution is wrong (or the
# build is already broken). Either way it is this library's problem.
# EMBEDS only, deliberately. An `include_str!` target that does not exist is a
# resolution bug (or an already-broken build), so the invariant is real there.
# It is NOT real for the umbrella: `crate_build_config_paths` is a fixed set of
# cargo/rustup filenames that a given repo may legitimately not have
# (`rustfmt.toml`, `rust-toolchain`, `.cargo`), and `build.rs` declares
# generated, gitignored inputs like `../dist`. Asserting existence over those
# would pin the absence of an optional config file.
#
# Space-joined, so iterate over WORDS. No path here holds a space; one that did
# would surface as missing rather than be silently skipped — the right
# direction for a pin.
real_embeds="$(embedded_outside_paths "$REPO_ROOT" HEAD | grep -v '^$' | sort -u | tr '\n' ' ')"
missing=""
for p in $real_embeds; do
    [ -e "$REPO_ROOT/$p" ] || missing="$missing $p"
done
check "every derived EMBED target exists in the tree" "" "$missing"

# A FLOOR, not an exact set. Deliberately not "these and no others": the whole
# reason this is derived rather than listed is that a NEW outside-tree embed is
# picked up automatically and needs no edit here. An exact-set pin would red
# this suite for a case that is already handled correctly — churn in exchange
# for nothing. What is worth pinning is that the six known targets, each of
# which reaches the crate through a DIFFERENT spelling or directory, never stop
# being seen.
for want in \
    examples/demo-workflows/setup_calculator.py \
    specs/pages \
    docs/runner-config-layers.md \
    .qontinui/ci.toml \
    .github/sibling-pins.conf \
    src/hooks/ui-bridge-events/useControlEvents.ts \
    crates/spec-check \
    crates/runner-stats \
    crates/runner-win32 \
    vendor/tao-0.35.0 \
    src/components/app/tab-types.ts \
    src/components/app/useAppNavigation.ts \
    Cargo.toml \
    Cargo.lock \
    rust-toolchain.toml \
    clippy.toml
do
    # Word-exact: a substring test would let `specs/pages-archive` satisfy
    # `specs/pages`. $real_out is space-joined and no path here holds a space.
    case " $real_out " in
        *" $want "*) pass_note "real repo: $want is in scope" ;;
        *) fail_note "real repo: $want is an input but NOT derived" ;;
    esac
done

echo
printf '%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
