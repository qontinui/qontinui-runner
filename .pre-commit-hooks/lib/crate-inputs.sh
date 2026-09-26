#!/usr/bin/env bash
#
# crate-inputs.sh — "which files OUTSIDE src-tauri/ does the cargo gate compile?"
#
# Sourced by `.pre-commit-hooks/cargo-prepush.sh`.
#
# THE PROBLEM THIS SOLVES
#
# The cargo gate scopes itself to "did anything under `src-tauri/` change?".
# That is not the crate's input set, and the gap is not small. The gate runs
# `cd src-tauri && cargo fmt` + `cargo clippy` — the same command CI runs in the
# required `test (ubuntu-22.04)` context — and that compiles THREE kinds of file
# the old pathspec could not see. All three are derived here, from the tree
# being pushed, never from a list.
#
#   1. PATH DEPENDENCIES INSIDE THIS REPO. `src-tauri/Cargo.toml` path-deps
#      `../crates/{spec-check,runner-stats,runner-win32}`. They are ordinary
#      workspace members, so a compile error in any of them fails the gate
#      outright — and `git log` already holds a commit touching ONLY that tree
#      (`2372b5de5 style(spec-check): apply cargo fmt to the crate`), which the
#      pathspec would have skipped with "no changes the crate compiles".
#
#   2. WHAT `build.rs` READS. `src-tauri/build.rs` reads
#      `../src/components/app/tab-types.ts` and `../src/components/app/useAppNavigation.ts`
#      and, in its own words, is "deliberately FATAL on failure" — it `panic!`s
#      on a missing file and `assert!`s on the shape of what it finds. A
#      frontend push that renames either one is a hard build failure that the
#      gate would have skipped. build.rs declares them to cargo itself with
#      `cargo:rerun-if-changed=`, which is exactly the statement "this is an
#      input", so that is what gets read.
#
#   3. `include_str!` / `include_bytes!` / `include_dir!` TARGETS. These resolve
#      relative to the FILE that names them, and plenty reach out of the tree.
#      Measured on origin/main 22b5380da, six targets outside `src-tauri/`:
#
#        examples/demo-workflows/*.py (3)  src/demo_workflows.rs:14-17   NOT a test
#        specs/pages                       src/spec_api/storage.rs:49    NOT a test
#        docs/runner-config-layers.md      src/config_report.rs:1907     #[test]
#        .qontinui/ci.toml                 src/ci_node/sibling.rs:1893   #[test]
#        .github/sibling-pins.conf         src/ci_node/sibling.rs:1894   #[test]
#        src/hooks/.../useControlEvents.ts src/mcp/ui_bridge/...:145     cfg(test) mod
#
#      `include_str!` of a missing path is a COMPILE error. The first two break
#      the gate's own plain `cargo clippy`; the other four break the `cargo test`
#      and all-targets clippy CI runs — which is the thing this hook mirrors, so
#      both classes gate.
#
# Plus the BUILD CONFIGURATION the gate's verdict depends on, which is a fixed
# set of cargo/rustup-defined filenames rather than a project list that drifts:
# the workspace root `Cargo.toml` (it owns `[profile.*]`), `Cargo.lock`,
# `rust-toolchain.toml` (which clippy), `clippy.toml` (the lint config the
# gate's verdict is literally read from), `rustfmt.toml` and `.cargo/`.
#
# WHY IT IS DERIVED AND NOT A LIST
#
# A hardcoded list covers what is there today and silently stops covering the
# next one. The plan that owns this work named three of the six embed targets;
# the other two-thirds were found by asking the source. So ask the source: every
# question here is answerable exactly, from the tree being pushed, at the cost
# of two `git grep`s. Same reasoning `cargo-prepush.sh`'s workspace preflight
# gives for probing with `cargo metadata` rather than listing the sibling path
# deps it knows about today.
#
# WHAT IT DELIBERATELY DOES NOT DO
#
# It does not decide whether a given embed is reachable from a non-test build.
# The distinction is real (see the table) but it does not change the answer: CI
# compiles the test targets, so both classes must run the gate. Deriving a
# cfg(test) reachability judgement from a grep would be guessing.
#
# It does not read `cargo metadata` for the workspace members, although that
# would be exact. `cargo metadata` needs the SIBLING qontinui-schemas checkout
# to resolve `src-tauri/Cargo.toml`'s path deps, and on a worktree without one
# it fails — which would turn every TS-only push on such a box into a full gate
# run, re-charging exactly the cost this hook exists to avoid. Grepping the
# manifests needs no sibling and no toolchain.
#
# ⚠️ UNRESOLVED MUST NOT BECOME THE NORMAL ANSWER. The readers are line-based,
# because squashing the 8 MB of Rust that names these macros into one string and
# running bash regexes over it costs more than the gate it is scoping. Two
# spellings do not fit on one line, and BOTH are in this repo's idiom, so both
# are handled rather than declared unknown: `concat!(env!("CARGO_MANIFEST_DIR"),
# "/rel")` (src/mcp/unified_workflows.rs:2979) and a literal on the line after
# the macro. If a single real invocation reported UNKNOWN on every push, the
# caller would fail open every time and this file would be dead weight that also
# re-charges every TS-only push the full gate — the exact cost
# qontinui-runner#1556 removed. A new unreadable spelling is therefore a
# REGRESSION to fix here, not an acceptable steady state; the test suite pins
# the real repo at zero unresolved invocations.

# A string literal directly inside the macro's parens.
EMBEDDED_PATHS_LITERAL_RE='^[[:space:]]*"([^"]+)"'
# `concat!(env!("CARGO_MANIFEST_DIR"), "/rel/path")` — the crate-root-relative
# idiom. Resolves against `src-tauri/`, which is CARGO_MANIFEST_DIR here.
EMBEDDED_PATHS_MANIFEST_RE='^[[:space:]]*concat![[:space:]]*\([[:space:]]*env![[:space:]]*\([[:space:]]*"CARGO_MANIFEST_DIR"[[:space:]]*\)[[:space:]]*,[[:space:]]*"([^"]+)"'
# `concat!(env!("OUT_DIR"), …)` — the build-script output directory. Cargo
# DEFINES it as a path outside the source tree, so a target under it is
# build-generated and is not a file a push can change. A deliberate SKIP, and
# an ALLOWLIST of one rather than "any env! is skippable": a user-defined
# variable could name a repo path, and guessing that it does not would
# understate the scope. Anything else reads as UNKNOWN and runs the gate.
EMBEDDED_PATHS_GENERATED_RE='^[[:space:]]*concat![[:space:]]*\([[:space:]]*env![[:space:]]*\([[:space:]]*"OUT_DIR"[[:space:]]*\)'
# An invocation opening. Deliberately requires the paren, so prose naming
# `include_str!` without calling it — this file's own header, for one — is not
# mistaken for one and does not read as UNKNOWN.
EMBEDDED_PATHS_CALL_RE='include(_(str|bytes|dir))?![[:space:]]*\('

# Print, one per line, the repo-relative paths embedded by `src-tauri/` Rust
# sources that resolve OUTSIDE `src-tauri/`. Each is a file or a directory; a
# caller matches a changed path against it by equality or by prefix.
#
# $1 — repo root
# $2 — the rev to read the sources at (the pushed tip, never the working tree:
#      a push that ADDS an embed must be judged by the source it is pushing)
#
# Returns 0 when every invocation it found was resolved, and 1 when ANYTHING
# was not — an unreadable tree, or a macro spelling this reader cannot follow.
# So an empty output with return 0 genuinely means "no outside-tree embeds",
# while return 1 means UNKNOWN, and the caller must tell those apart. Running
# the gate is the safe reading of the second.
embedded_outside_paths() {
    local repo="$1" rev="$2" line file lineno content
    local grep_rc=0 out unresolved=0

    # `-I` skips binaries. The pathspec glob matches across `/` — git pathspecs
    # are fnmatch WITHOUT FNM_PATHNAME — so this reaches every .rs in the tree.
    out="$(git -C "$repo" grep -I -n -E 'include(_(str|bytes|dir))?!' "$rev" \
        -- 'src-tauri/*.rs' 2>/dev/null)" || grep_rc=$?
    # git grep exits 1 for "no matches", which is a real answer, and >1 for a
    # genuine failure, which is not.
    if [ "$grep_rc" -gt 1 ]; then
        return 1
    fi

    while IFS= read -r line; do
        [ -n "$line" ] || continue
        # `<rev>:<path>:<line>:<content>`. Strip the rev we asked for rather
        # than cutting on the first colon: a rev is never empty here, and a
        # path could in principle hold one.
        line="${line#"$rev":}"
        file="${line%%:*}"
        content="${line#*:}"
        lineno="${content%%:*}"
        content="${content#*:}"
        [ -n "$file" ] || continue

        [[ "$content" =~ $EMBEDDED_PATHS_CALL_RE ]] || continue

        if embedded_paths_scan "$file" "$content"; then
            continue
        fi
        # The literal is not on this line. Re-read a small window from the
        # blob and retry — this is the rare path (one site in this repo), so
        # the extra `git show` is paid ~never, and it keeps a continuation
        # line or a `concat!` block from reading as UNKNOWN forever.
        case "$lineno" in
            ''|*[!0-9]*) unresolved=1; continue ;;
        esac
        local window
        window="$(git -C "$repo" show "$rev:$file" 2>/dev/null \
            | sed -n "${lineno},$((lineno + 4))p" | tr '\n' ' ')" || window=""
        if [ -n "$window" ] && embedded_paths_scan "$file" "$window"; then
            continue
        fi
        unresolved=1
    done <<EOF
$out
EOF
    [ "$unresolved" -eq 0 ] || return 1
    return 0
}

# Scan one piece of text for macro invocations, printing every OUTSIDE-tree
# path it resolves. Returns 0 when it resolved at least one literal, 1 when the
# text opened an invocation it could not read.
#
# $1 — the .rs file the text came from (relative paths resolve against its dir)
# $2 — the text
embedded_paths_scan() {
    local file="$1" text="$2" literal resolved found=0 unreadable=0

    while [[ "$text" =~ $EMBEDDED_PATHS_CALL_RE ]]; do
        # Always advance past the opening, so an unreadable invocation cannot
        # spin this loop.
        text="${text#*"${BASH_REMATCH[0]}"}"

        if [[ "$text" =~ $EMBEDDED_PATHS_LITERAL_RE ]]; then
            literal="${BASH_REMATCH[1]}"
            case "$literal" in
                # The only interpolation cargo defines for these macros that
                # names a repo path. Everything else ($OUT_DIR and friends) is
                # build-generated and is not a file in this tree.
                '$CARGO_MANIFEST_DIR/'*) resolved="src-tauri/${literal#'$CARGO_MANIFEST_DIR/'}" ;;
                *'$'*) found=1; continue ;;
                /*) found=1; continue ;;          # absolute: not a repo path
                *) resolved="$(dirname "$file")/$literal" ;;
            esac
        elif [[ "$text" =~ $EMBEDDED_PATHS_MANIFEST_RE ]]; then
            literal="${BASH_REMATCH[1]}"
            resolved="src-tauri/${literal#/}"
        elif [[ "$text" =~ $EMBEDDED_PATHS_GENERATED_RE ]]; then
            found=1
            continue
        else
            # An invocation this reader cannot follow. It must NOT be masked by
            # a sibling on the same line or in the same window that DID resolve
            # — `found` alone would report the whole text "fully resolved" and
            # the caller would take the scope-is-complete branch on an
            # understated scope. That is the one direction this file must never
            # fail in.
            unreadable=1
            continue
        fi
        found=1

        resolved="$(embedded_paths_normalize "$resolved")"
        [ -n "$resolved" ] || continue
        # Inside the tree is already covered by the src-tauri/ scope.
        case "$resolved" in
            src-tauri/*|src-tauri) continue ;;
        esac
        printf '%s\n' "$resolved"
    done
    { [ "$unreadable" -eq 0 ] && [ "$found" -eq 1 ]; } || return 1
    return 0
}

# Collapse `.` and `..` segments in a repo-relative path. Prints nothing when
# the path escapes the repo root, which a caller reads as "not a repo path".
embedded_paths_normalize() {
    local path="$1" seg out="" depth=0
    local -a parts=()
    local IFS=/
    # Word-split on `/` only. Globbing is disabled across the assignment
    # because an unquoted expansion in an array assignment is ALSO
    # glob-expanded, and a literal holding `*` would otherwise be replaced by
    # whatever happens to be on disk. Saved and restored rather than ending on
    # a bare `set +f`: this is a sourced library, and a caller that had
    # globbing off would silently get it back on.
    local glob_was_off=0
    case "$-" in *f*) glob_was_off=1 ;; esac
    set -f
    # shellcheck disable=SC2206 # deliberate word splitting on IFS=/
    local -a raw=($path)
    [ "$glob_was_off" -eq 1 ] || set +f
    IFS=' '
    for seg in ${raw[@]+"${raw[@]}"}; do
        case "$seg" in
            ""|.) continue ;;
            ..)
                [ "$depth" -gt 0 ] || return 0   # escaped the repo root
                depth=$((depth - 1))
                ;;
            *)
                parts[$depth]="$seg"
                depth=$((depth + 1))
                ;;
        esac
    done
    [ "$depth" -gt 0 ] || return 0
    out="${parts[0]}"
    local i
    for ((i = 1; i < depth; i++)); do
        out="$out/${parts[i]}"
    done
    printf '%s\n' "$out"
}

# True when $1 is an EMBED target (equal to one, or inside an `include_dir!`
# tree), given the embed set as $2...
#
# This is what decides whether a content edit to an outside-tree path can be
# excused, and it is deliberately NOT "is it outside src-tauri/". The scope now
# also carries path-dependency crates, build.rs inputs and the build config,
# and a content edit to any of THOSE changes what compiles or what the verdict
# is read from. Only an embed — whose sole route into the build is the embed
# itself — is excusable, and then only when its blob is still valid UTF-8.
crate_path_is_embed_only() {
    local path="$1" embed
    shift
    for embed in "$@"; do
        [ -n "$embed" ] || continue
        # String comparison, never a `case` pattern: an embed literal holding
        # `*`, `?` or `[` would otherwise GLOB-match unrelated paths and
        # over-EXCUSE them. `embedded_paths_normalize` guards the same class on
        # the way in; this is the matching guard on the way out.
        [ "$path" != "$embed" ] || return 0
        [ "${path#"$embed"/}" = "$path" ] || return 0
    done
    return 1
}

# In-repo `path = "..."` dependency directories, as declared by this repo's own
# Cargo.toml manifests at $2. Resolved relative to the manifest that names them
# and emitted only when they land inside the repo and outside `src-tauri/` —
# `../../qontinui-schemas/rust` escapes the repo (a different repo's push is not
# ours to scope), `./clorinde` and the `[[bin]] path =` entries stay inside
# src-tauri/ and are already covered.
#
# Grep, not `cargo metadata`: see the header for why the exact answer is the
# wrong trade here.
#
# $1 — repo root   $2 — rev
# Returns 0 when the grep ran, 1 when it could not (UNKNOWN).
crate_path_dep_dirs() {
    local repo="$1" rev="$2" line file value resolved grep_rc=0 out manifests

    # EVERY manifest dir in the repo, not only the ones something path-deps.
    # Cargo loads every `[workspace] members` manifest to build the workspace,
    # so a syntax error in a member NOTHING depends on still fails the gate —
    # measured: breaking a non-dependency member's Cargo.toml makes
    # `cargo clippy` from a sibling member exit 101. This repo has three such
    # members (`crates/comprehension`, `crates/qontinui-app-generator`,
    # `crates/qontinui-backend-generator`), and the root `Cargo.toml` pathspec
    # does NOT cover them: a git pathspec of `Cargo.toml` matches the root file
    # alone. Emitting the dirname of each manifest costs nothing — the grep
    # below already walks them — and needs no `[workspace] members` parsing.
    manifests="$(git -C "$repo" ls-tree -r --name-only "$rev" 2>/dev/null \
        | grep -E '(^|/)Cargo\.toml$')" || manifests=""
    while IFS= read -r file; do
        [ -n "$file" ] || continue
        resolved="$(embedded_paths_normalize "$(dirname "$file")")"
        [ -n "$resolved" ] || continue
        case "$resolved" in
            src-tauri/*|src-tauri) continue ;;
        esac
        printf '%s\n' "$resolved"
    done <<EOF
$manifests
EOF

    out="$(git -C "$repo" grep -I -n -E '^[[:space:]]*(path|.*\{[^}]*path)[[:space:]]*=[[:space:]]*"' "$rev" \
        -- '*Cargo.toml' 2>/dev/null)" || grep_rc=$?
    [ "$grep_rc" -le 1 ] || return 1

    while IFS= read -r line; do
        [ -n "$line" ] || continue
        line="${line#"$rev":}"
        file="${line%%:*}"
        value="${line#*:}"
        value="${value#*:}"
        [ -n "$file" ] || continue
        # Every `path = "..."` on the line. A manifest line carries at most one
        # in practice, but a loop costs nothing and cannot under-read.
        while [[ "$value" =~ path[[:space:]]*=[[:space:]]*\"([^\"]+)\" ]]; do
            resolved="${BASH_REMATCH[1]}"
            value="${value#*"${BASH_REMATCH[0]}"}"
            # A DEPENDENCY path is always a directory. `path = "…/foo.rs"` is a
            # `[[bin]]` / `[[example]]` TARGET path, which names a file inside a
            # crate we have already emitted the directory for — `vendor/tao`'s
            # manifest alone carries 30 of them.
            case "$resolved" in *.rs) continue ;; esac
            resolved="$(embedded_paths_normalize "$(dirname "$file")/$resolved")"
            [ -n "$resolved" ] || continue
            case "$resolved" in
                src-tauri/*|src-tauri) continue ;;
            esac
            printf '%s\n' "$resolved"
        done
    done <<EOF
$out
EOF
    return 0
}

# What `src-tauri/build.rs` reads from outside its own directory.
#
# build.rs declares its inputs to cargo with `cargo:rerun-if-changed=`, which is
# the authoritative statement "this file is an input" — but two of them are
# printed through a `const`, so the declaration line itself carries no path.
# Rather than resolve the interpolation, take the `../`-prefixed string literals
# in the file: build.rs's whole job is reading repo files, so a literal that
# climbs out of `src-tauri/` IS a repo path.
#
# ⚠️ FILE-LIKE LITERALS ONLY, and that bound is the point rather than a
# limitation. Over-inclusion is safe for CORRECTNESS and fatal for the FEATURE:
# one `"../src"` or `"../docs"` literal — even inside a comment, since this is a
# grep — would put a whole tree in the pathspec and make the gate run on every
# push, which is invisible because a gate that runs looks exactly like a gate
# that was needed. Requiring a dot in the last segment means a DIRECTORY literal
# can never be emitted, so the blast radius of a bad literal is one file. The
# cost is that a genuine directory input would be missed; `../dist`, the only
# one today, is generated and gitignored, so it could never appear in a diff.
#
# $1 — repo root   $2 — rev
# Returns 0 when the file was read, 1 when it could not be (UNKNOWN). A repo
# with no `src-tauri/build.rs` is NOT unknown: it is an empty answer.
crate_build_script_inputs() {
    local repo="$1" rev="$2" body line literal resolved

    git -C "$repo" cat-file -e "$rev:src-tauri/build.rs" 2>/dev/null || return 0
    body="$(git -C "$repo" show "$rev:src-tauri/build.rs" 2>/dev/null)" || return 1

    while IFS= read -r line; do
        while [[ "$line" =~ \"(\.\./[^\"]*)\" ]]; do
            literal="${BASH_REMATCH[1]}"
            line="${line#*"${BASH_REMATCH[0]}"}"
            resolved="$(embedded_paths_normalize "src-tauri/$literal")"
            [ -n "$resolved" ] || continue
            case "$resolved" in
                src-tauri/*|src-tauri) continue ;;
            esac
            # File-like only — see the ⚠️ above. A leading dot does not count
            # (`.git` is a directory), so the test is "a dot that is not the
            # first character of the basename".
            case "${resolved##*/}" in
                .*)      continue ;;   # `.git` &c: a dotted DIRECTORY name
                *.*)     ;;            # a dot inside the name: file-like
                *)       continue ;;   # no dot at all: a directory
            esac
            printf '%s\n' "$resolved"
        done
    done <<EOF
$body
EOF
    return 0
}

# The build configuration the gate's verdict depends on. A FIXED set, and
# legitimately so: these filenames are defined by cargo and rustup, not by this
# project, so the list cannot drift the way a project inventory would. A path
# absent from the tree is harmless — `git diff -- <missing>` simply matches
# nothing.
crate_build_config_paths() {
    printf '%s\n' Cargo.toml Cargo.lock rust-toolchain.toml rust-toolchain \
        clippy.toml rustfmt.toml .rustfmt.toml .cargo
}

# Every path OUTSIDE `src-tauri/` that the cargo gate compiles or reads, from
# all four sources above, de-duplicated.
#
# $1 — repo root
# $2 — the rev to read at (the pushed tip, never the working tree: a push that
#      ADDS an input must be judged by the source it is pushing)
#
# Returns 0 only when EVERY source answered completely. A 1 is UNKNOWN — the
# scope would be understated — and the caller must run the gate rather than
# treat the output as the whole answer.
# Also sets CRATE_INPUT_EMBEDS to the embed subset, so a caller that needs both
# (the excuse predicate does) pays ONE `git grep` rather than two — measured at
# ~0.6 s each on this repo, on the hot path this whole hook exists to keep
# cheap, and multiplied by every ref in a multi-ref push.
CRATE_INPUT_EMBEDS=""
crate_input_paths() {
    local repo="$1" rev="$2" ok=0 out=""
    local embeds deps build_inputs

    embeds="$(embedded_outside_paths "$repo" "$rev")" || ok=1
    CRATE_INPUT_EMBEDS="$embeds"
    deps="$(crate_path_dep_dirs "$repo" "$rev")" || ok=1
    build_inputs="$(crate_build_script_inputs "$repo" "$rev")" || ok=1

    out="$(printf '%s\n%s\n%s\n%s\n' \
        "$embeds" "$deps" "$build_inputs" "$(crate_build_config_paths)" \
        | grep -v '^$' | sort -u)"
    [ -z "$out" ] || printf '%s\n' "$out"
    return "$ok"
}
