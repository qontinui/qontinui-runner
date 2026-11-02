#!/usr/bin/env bash
#
# gen-events-file-attribution.sh — "which of the drifted FILES could this push
# have moved?"
#
# Sourced by `.pre-commit-hooks/gen-events-drift.sh`, after
# `lib/gen-events-attribution.sh` has answered the coarser question, "does this
# push touch ANY codegen input?". Tested, without a Rust toolchain, by
# `gen-events-file-attribution-test.sh`.
#
# THE PROBLEM THIS SOLVES
#
# The bindings in `qontinui-schemas/ts/src/generated/` are regularly AHEAD of
# runner main. qontinui-schemas checks a type's binding in first, because the
# runner's `add!` cannot compile until the type is on its pinned sibling. It
# declares the wait in `codegen/pending-registration.toml`, and the runner
# half lands days later. Measured on runner `origin/main` `c21297b9d` on
# 2026-10-30 against schemas `3713b8e2b`: 14 `spec_model` bindings plus their
# `index.ts` lines, 15 files in all, waiting on a runner PR that had stalled.
#
# The whole-push attribution can only say "this push touches `src-tauri/src/`,
# so all 15 files are yours". Every push touching runner Rust was refused for
# drift it could not have caused, and went out with `SKIP=gen-events-drift`.
# That happened four times in one week (coord findings 151931cf, 79415d3d,
# 019350f7, 19f8a714), and once a hook is skipped routinely it stops being read.
#
# THE RULE, per drifted file
#
#   presence  `<T>.d.ts` on ONE side only. Which files exist is decided by the
#             registration set alone. So the file is this push's only when the
#             push adds or removes T's `add!` registration: the registry is
#             read at the merge-base and in the working tree, by
#             `lib/gen-events-registry.mjs`, and the two sets are compared. A
#             registry-ADJACENT input also counts (the exporter binary,
#             `generate_types.sh`, a `schema_export/` directory), since it can
#             change the set without an `add!` line changing.
#   barrel    `index.ts`: one export line per registered name, so the same rule
#             applies line by line. Any other line changing counts as
#             registry-adjacent.
#   content   `<T>.d.ts` on both sides, different. If every path root in T's
#             registered type resolves to the qontinui-schemas crates, only a
#             SHAPE input can move it. Every codegen input is a shape input
#             EXCEPT a source file of the runner crate itself (under
#             `src-tauri/src/`). If T is defined in this repo, any codegen
#             input can move it, and that is the whole-push rule.
#   other     any other file in the tree counts against the push.
#
# The trade is the same asymmetric one `lib/gen-events-attribution.sh` makes.
# A file is cleared only on positive evidence that the push cannot reach it.
# Every doubt falls back to the WHOLE-PUSH verdict that the hook gave before
# this file existed. The doubts are:
#   - the registry cannot be read (see the premises in the parser's header);
#   - `lib.rs` does not declare `pub mod schema_export;` plainly, in the
#     working tree or (when this change touches it) at the merge-base, or it
#     rebinds a crate name the registry reader trusts;
#   - the parsed working-tree names differ from the set of files the
#     regeneration actually emitted;
#   - the registry changed in this push and its merge-base version cannot be
#     read, or its `add!` macro changed;
#   - a directory appears in either tree;
#   - `node` is missing.
#
# WHAT IT DOES NOT SEE, stated so nobody mistakes it for a proof:
#   - a runner module that changes an external type's schema at RUNTIME. That
#     would mean code reached from `export_schemas` main (e.g.
#     `instance_env`) mutating state that a qontinui-schemas `JsonSchema`
#     impl reads while it builds its schema. No impl does that today, and
#     only regenerating at the merge-base would catch it. Regenerating there
#     was rejected because it doubles a release build that measured 2m32s.
#   - a qontinui-schemas checkout whose Rust and TS halves disagree. That is
#     drift, but no runner push can cause it or clear it, and the
#     content rule rightly calls it pre-existing.

GEN_EVENTS_FILE_ATTRIBUTION_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GEN_EVENTS_REGISTRY_PARSER="$GEN_EVENTS_FILE_ATTRIBUTION_LIB_DIR/gen-events-registry.mjs"
GEN_EVENTS_REGISTRY_FILE="src-tauri/src/schema_export.rs"

# Inputs that can change WHICH names are exported without an `add!` line in
# the registry changing. `lib.rs` is decided separately, by its premise below.
GEN_EVENTS_REGISTRY_ADJACENT=(
    "src-tauri/src/bin/export_schemas.rs"
    "src-tauri/scripts/generate_types.sh"
)

# Node needs a native path on Windows. Git Bash hands `/c/...` to a native
# program unchanged, and `node` reads it as a relative path. The same
# conversion `generate_types.sh` does.
_gen_events_native_path() {
    if command -v cygpath >/dev/null 2>&1; then
        cygpath -w "$1"
    else
        printf '%s\n' "$1"
    fi
}

# Parse the registry file $1 into GEN_EVENTS_REGISTRY_MACRO (the `add!` body)
# and GEN_EVENTS_REGISTRY_ROWS (`<name>\t<external|local>` lines). Returns 0
# on success. Otherwise it returns 1, with the reason in
# GEN_EVENTS_REGISTRY_ERROR.
gen_events_registry_parse() {
    local file="$1" out rc=0
    GEN_EVENTS_REGISTRY_MACRO=""
    GEN_EVENTS_REGISTRY_ROWS=""
    GEN_EVENTS_REGISTRY_ERROR=""
    if ! command -v node >/dev/null 2>&1; then
        GEN_EVENTS_REGISTRY_ERROR="node is not on PATH, so the registry cannot be read"
        return 1
    fi
    if [ ! -f "$GEN_EVENTS_REGISTRY_PARSER" ]; then
        GEN_EVENTS_REGISTRY_ERROR="missing $GEN_EVENTS_REGISTRY_PARSER"
        return 1
    fi
    # stderr apart from stdout: a Node warning on stderr must not become a row.
    local errf err=""
    errf="$(mktemp "${TMPDIR:-/tmp}/gen-events-registry-err.XXXXXX")" || {
        GEN_EVENTS_REGISTRY_ERROR="could not create a temp file for the registry parser"
        return 1
    }
    out="$(node "$(_gen_events_native_path "$GEN_EVENTS_REGISTRY_PARSER")" \
        "$(_gen_events_native_path "$file")" 2>"$errf")" || rc=$?
    err="$(head -n 1 "$errf" 2>/dev/null || true)"
    rm -f "$errf"
    if [ "$rc" -ne 0 ]; then
        case "$out" in
            "@violation "*) GEN_EVENTS_REGISTRY_ERROR="${out#@violation }" ;;
            *)              GEN_EVENTS_REGISTRY_ERROR="the registry parser failed (exit $rc): ${err:-no message}" ;;
        esac
        return 1
    fi
    case "$out" in
        "@macro "*) ;;
        *) GEN_EVENTS_REGISTRY_ERROR="the registry parser printed no @macro line"; return 1 ;;
    esac
    GEN_EVENTS_REGISTRY_MACRO="${out%%$'\n'*}"
    GEN_EVENTS_REGISTRY_ROWS="$(printf '%s\n' "$out" | sed '1d;/^$/d')"
    if [ -z "$GEN_EVENTS_REGISTRY_ROWS" ]; then
        GEN_EVENTS_REGISTRY_ERROR="the registry parser found no registrations"
        return 1
    fi
    return 0
}

# Does `lib.rs` (content on stdin) declare the registry module plainly? It must
# hold `pub mod schema_export;` and no `path = ...` anywhere. That covers
# `#[path = "x.rs"]`, `#[cfg_attr(.., path = "x.rs")]` and its rustfmt-wrapped
# form, any of which could point `schema_export` at a file the registry reader
# never sees. The check is deliberately coarse: a `path =` in a comment or in
# unrelated code also fails it, and failing only costs the per-file rule.
#
# It also refuses an `extern crate .. as <a crate the registry reader trusts>`,
# which would make that name mean something else across the whole crate. This
# crate does use the form (`extern crate self as qontinui_runner_lib`), so only
# the trusted names are refused.
#
# `grep >/dev/null`, never `grep -q`: the caller runs under pipefail, and a
# `-q` that exits on its first match can SIGPIPE the writer and turn a match
# into a failed pipeline.
_gen_events_lib_rs_premise() {
    local content
    content="$(cat)" || return 1
    printf '%s\n' "$content" | grep -E '^[[:space:]]*pub[[:space:]]+mod[[:space:]]+schema_export[[:space:]]*;' >/dev/null || return 1
    if printf '%s\n' "$content" | grep -E '(^|[^A-Za-z0-9_])path[[:space:]]*=' >/dev/null; then
        return 1
    fi
    if printf '%s\n' "$content" | tr '\n' ' ' \
        | grep -E '(^|[^A-Za-z0-9_])extern[[:space:]]+crate[[:space:]]+[A-Za-z0-9_]+[[:space:]]+as[[:space:]]+(qontinui_types|qontinui_vision_core|serde_json|schemars)([^A-Za-z0-9_]|$)' >/dev/null; then
        return 1
    fi
    return 0
}

# The display label of one attribution source, worded for the stage. These
# are the labels `gen_events_render_mine` prints beside each input.
_gen_events_source_label() {
    case "$1:$2" in
        push:committed)      echo "committed in this push" ;;
        push:staged)         echo "staged, not committed — not part of this push" ;;
        push:unstaged)       echo "uncommitted changes — not part of this push" ;;
        push:untracked)      echo "untracked — not part of this push" ;;
        push:staged-later)   echo "staged for a later commit — not part of this push" ;;
        commit:committed)    echo "already committed on this branch" ;;
        commit:staged)       echo "staged for this commit" ;;
        commit:unstaged)     echo "unstaged — not in this commit" ;;
        commit:untracked)    echo "untracked — not in this commit" ;;
        commit:staged-later) echo "staged for a later commit — not in this commit" ;;
        *)                   echo "source unknown: '$2'" ;;
    esac
}

# `<path> (<label>[; <label>])` for an input path $1, from
# ATTRIBUTION_TOUCHED_DETAIL. A reason names the input with its label, so the
# reader can see whether the change is in what is being pushed or only in the
# working tree. Saying "this push changes it" about a dirty file the push does
# not carry is the false claim #1667 met.
_gen_events_input_with_label() {
    local want="$1" stage="$2" p src labels=""
    while IFS=$'\t' read -r p src; do
        [ "$p" = "$want" ] || continue
        labels+="${labels:+; }$(_gen_events_source_label "$stage" "$src")"
    done <<< "${ATTRIBUTION_TOUCHED_DETAIL:-}"
    printf '%s (%s)' "$want" "${labels:-source unknown}"
}

# A touched input that can reach the export ONLY through a type defined in
# this repo: a source file of the runner crate itself, under `src-tauri/src/`,
# other than the registry files. Everything else in the codegen input list is a
# SHAPE input. That covers manifests, build scripts, `vendor/`, the toolchain,
# cargo config, the lockfile and `generate_types.sh`. It also covers the
# SOURCES of the other in-repo crates (`src-tauri/clorinde/`, `crates/*`).
# Those are deliberately not source-only: a `[patch]` can point a crates.io
# crate at any in-repo directory. That crate's source can then shape a
# qontinui-schemas type's schema, which a directory rule could not tell from
# an ordinary path dependency. The precise rule is kept for the one directory
# almost every runner push touches.
_gen_events_is_source_only_input() {
    local p="$1" entry
    case "$p" in
        "$GEN_EVENTS_REGISTRY_FILE"|src-tauri/src/schema_export/*) return 1 ;;
    esac
    for entry in "${GEN_EVENTS_REGISTRY_ADJACENT[@]}"; do
        [ "$p" = "$entry" ] && return 1
    done
    case "$p" in
        src-tauri/src/*) return 0 ;;
    esac
    return 1
}

# The plain-file listing of a generated tree: one name per line, sorted.
# Returns 1 when the tree holds a directory or anything else unexpected. The
# rule set is per-file, so a nested tree has no defined answer here.
_gen_events_tree_listing() {
    local dir="$1" f name
    for f in "$dir"/* "$dir"/.[!.]*; do
        [ -e "$f" ] || [ -L "$f" ] || continue
        name="${f##*/}"
        [ -f "$f" ] || return 1
        printf '%s\n' "$name"
    done | LC_ALL=C sort
    return "${PIPESTATUS[0]}"
}

# One `git diff --no-index` over the two trees, as `<A|D|M>\t<file name>` lines
# on stdout. A=only in the regeneration, D=only in the baseline, M=in both,
# different. Returns 1 when the diff itself failed (status above 1) or
# reported a status this rule set has no answer for. It is one process, not one
# `cmp` per file: a Windows box pays dearly for each spawn.
_gen_events_tree_diff() {
    local rec recs=() n i status
    while IFS= read -r -d '' rec; do recs+=("$rec"); done < <(
        git diff --no-index --no-renames --name-status -z -- "$1" "$2" 2>/dev/null
        printf 'rc=%d\0' "$?")
    n="${#recs[@]}"
    [ "$n" -gt 0 ] || return 1
    case "${recs[$((n - 1))]}" in rc=0|rc=1) ;; *) return 1 ;; esac
    [ $(( (n - 1) % 2 )) -eq 0 ] || return 1
    for ((i = 0; i < n - 1; i += 2)); do
        status="${recs[$i]}"
        case "$status" in A|D|M) ;; *) return 1 ;; esac
        printf '%s\t%s\n' "$status" "${recs[$((i + 1))]##*/}"
    done
    return 0
}

# Is $1 one of the newline-separated names in $2? Pattern matching, not a
# pipe into `grep -q`, for the pipefail reason given above.
_gen_events_name_in() {
    case $'\n'"$2"$'\n' in
        *$'\n'"$1"$'\n'*) return 0 ;;
    esac
    return 1
}

# Non-empty lines on stdin, as a number. `grep -c` exits 1 on a count of zero,
# which under the hook's `set -e` would end the hook inside an assignment.
_gen_events_count_lines() {
    awk 'NF { n++ } END { print n + 0 }'
}

# `export type { T } from './T';` lines of an index.ts ($1), one name per line,
# sorted. The other lines go to stdout of `_gen_events_barrel_rest`.
_gen_events_barrel_names() {
    [ -f "$1" ] || return 0
    sed -n "s/^export type { \([A-Za-z_][A-Za-z0-9_]*\) } from '\.\/\1';\r\{0,1\}\$/\1/p" "$1" | LC_ALL=C sort -u
}
_gen_events_barrel_rest() {
    [ -f "$1" ] || { printf '<absent>\n'; return 0; }
    grep -vE "^export type \{ [A-Za-z_][A-Za-z0-9_]* \} from '\./[A-Za-z_][A-Za-z0-9_]*';"$'\r'"?\$" "$1" || true
}

# Decide per drifted file. Call only after `gen_events_attribution` returned
# `mine` for the repo at $1. $2 is the baseline tree and $3 the regenerated
# one. Sets:
#
#   FILE_ATTRIBUTION_STATE   mine | pre-existing | whole-push
#   FILE_ATTRIBUTION_REASON  why per-file attribution could not run, set only
#                            for `whole-push`; the caller keeps its whole-push
#                            verdict
#   FILE_ATTRIBUTION_MINE    `<file>\t<why>` lines: drift this push can move
#   FILE_ATTRIBUTION_PRE     `<file>\t<why>` lines: drift it cannot
#   FILE_ATTRIBUTION_AHEAD   how many of those are bindings AHEAD of the runner
#                            (registered on neither side). Regenerating would
#                            delete them, so the hook's remedy says so.
gen_events_file_attribution() {
    local repo="$1" baseline="$2" regen="$3"
    FILE_ATTRIBUTION_STATE="whole-push"
    FILE_ATTRIBUTION_REASON=""
    FILE_ATTRIBUTION_MINE=""
    FILE_ATTRIBUTION_PRE=""
    FILE_ATTRIBUTION_AHEAD=0
    # "push" or "commit", for the reasons below: the hook runs at both stages.
    local stage
    stage="$(gen_events_stage)"

    if [ "${ATTRIBUTION_STATE:-}" != "mine" ] || [ -z "${ATTRIBUTION_BASE_SHA:-}" ]; then
        FILE_ATTRIBUTION_REASON="the whole-push attribution did not run, or did not return 'mine'"
        return 0
    fi

    # ── what the push touched, by class ────────────────────────────────────
    local p registry_touched=0 adjacent="" shape="" lib_touched=0 entry
    while IFS= read -r p; do
        [ -n "$p" ] || continue
        if [ "$p" = "$GEN_EVENTS_REGISTRY_FILE" ]; then
            registry_touched=1
        fi
        case "$p" in src-tauri/src/schema_export/*) adjacent+="$p"$'\n' ;; esac
        for entry in "${GEN_EVENTS_REGISTRY_ADJACENT[@]}"; do
            [ "$p" = "$entry" ] && adjacent+="$p"$'\n'
        done
        [ "$p" = "src-tauri/src/lib.rs" ] && lib_touched=1
        _gen_events_is_source_only_input "$p" || shape+="$p"$'\n'
    done <<< "$ATTRIBUTION_TOUCHED"

    # The registry reader trusts that `schema_export` IS schema_export.rs.
    # lib.rs is checked in the working tree on every decision, because a
    # `#[path]` already on main would redirect it with lib.rs untouched. When
    # this change touches lib.rs, the merge-base copy is checked too. Either
    # failing means the registry the reader parses may not be the one that
    # compiled, and nothing per-file can be concluded.
    if ! _gen_events_lib_rs_premise 2>/dev/null < "$repo/src-tauri/src/lib.rs"; then
        FILE_ATTRIBUTION_REASON="src-tauri/src/lib.rs does not declare \`pub mod schema_export;\` plainly (or carries a path attribute), so the registry file may not be the one that compiled"
        return 0
    fi
    if [ "$lib_touched" = "1" ] && ! git -C "$repo" show "$ATTRIBUTION_BASE_SHA:src-tauri/src/lib.rs" 2>/dev/null \
            | _gen_events_lib_rs_premise; then
        FILE_ATTRIBUTION_REASON="src-tauri/src/lib.rs at the merge-base ${ATTRIBUTION_BASE_SHA:0:12} does not declare \`pub mod schema_export;\` plainly, so the registries are not comparable"
        return 0
    fi
    adjacent="$(printf '%s' "$adjacent" | LC_ALL=C sort -u | sed '/^$/d')"
    shape="$(printf '%s' "$shape" | LC_ALL=C sort -u | sed '/^$/d')"

    # ── the two trees ──────────────────────────────────────────────────────
    local base_list regen_list
    if ! base_list="$(_gen_events_tree_listing "$baseline")"; then
        FILE_ATTRIBUTION_REASON="$baseline holds something other than plain files"
        return 0
    fi
    if ! regen_list="$(_gen_events_tree_listing "$regen")"; then
        FILE_ATTRIBUTION_REASON="the regenerated tree holds something other than plain files"
        return 0
    fi

    # ── the registry, in the working tree the regeneration compiled ─────────
    if ! gen_events_registry_parse "$repo/$GEN_EVENTS_REGISTRY_FILE"; then
        FILE_ATTRIBUTION_REASON="the registry in this tree cannot be read: $GEN_EVENTS_REGISTRY_ERROR"
        return 0
    fi
    local wt_rows="$GEN_EVENTS_REGISTRY_ROWS" wt_macro="$GEN_EVENTS_REGISTRY_MACRO"
    local wt_names regen_names
    wt_names="$(printf '%s\n' "$wt_rows" | cut -f1 | LC_ALL=C sort -u)"
    regen_names="$(printf '%s\n' "$regen_list" | sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\)\.d\.ts$/\1/p' | LC_ALL=C sort -u)"
    if [ "$wt_names" != "$regen_names" ]; then
        local extra
        local n_parsed n_generated
        extra="$(LC_ALL=C comm -3 <(printf '%s\n' "$wt_names") <(printf '%s\n' "$regen_names") | sed 's/^[[:space:]]*//' | awk 'NF && n < 3 { printf "%s ", $0; n++ }')"
        n_parsed="$(printf '%s\n' "$wt_names" | _gen_events_count_lines)"
        n_generated="$(printf '%s\n' "$regen_names" | _gen_events_count_lines)"
        FILE_ATTRIBUTION_REASON="the registry parse does not account for what the regeneration emitted ($n_parsed names parsed, $n_generated bindings generated; first differences: ${extra% })"
        return 0
    fi

    # ── the registration set at the merge-base, when the push changed it ─────
    local changed=""
    if [ "$registry_touched" = "1" ]; then
        local base_file
        base_file="$(mktemp "${TMPDIR:-/tmp}/gen-events-registry-base.XXXXXX")" || {
            FILE_ATTRIBUTION_REASON="could not create a temp file for the merge-base registry"
            return 0
        }
        if ! git -C "$repo" show "$ATTRIBUTION_BASE_SHA:$GEN_EVENTS_REGISTRY_FILE" > "$base_file" 2>/dev/null; then
            rm -f "$base_file"
            FILE_ATTRIBUTION_REASON="$GEN_EVENTS_REGISTRY_FILE could not be read at the merge-base ${ATTRIBUTION_BASE_SHA:0:12}"
            return 0
        fi
        if ! gen_events_registry_parse "$base_file"; then
            rm -f "$base_file"
            FILE_ATTRIBUTION_REASON="the registry at the merge-base ${ATTRIBUTION_BASE_SHA:0:12} cannot be read: $GEN_EVENTS_REGISTRY_ERROR"
            return 0
        fi
        rm -f "$base_file"
        if [ "$GEN_EVENTS_REGISTRY_MACRO" != "$wt_macro" ]; then
            FILE_ATTRIBUTION_REASON="the add! macro differs from the merge-base's, so names there are not comparable"
            return 0
        fi
        local base_names
        base_names="$(printf '%s\n' "$GEN_EVENTS_REGISTRY_ROWS" | cut -f1 | LC_ALL=C sort -u)"
        # Names registered on exactly one side: what this push added or removed.
        changed="$(LC_ALL=C comm -3 <(printf '%s\n' "$base_names") <(printf '%s\n' "$wt_names") | sed 's/^[[:space:]]*//;/^$/d')"
    fi
    local drift
    if ! drift="$(_gen_events_tree_diff "$baseline" "$regen")"; then
        FILE_ATTRIBUTION_REASON="git diff --no-index over the two trees failed, or reported a status other than A, D or M"
        return 0
    fi

    local first_adjacent="${adjacent%%$'\n'*}" first_shape="${shape%%$'\n'*}"
    local registry_label="" adjacent_label="" shape_label=""
    [ "$registry_touched" = "1" ] && registry_label="$(_gen_events_input_with_label "$GEN_EVENTS_REGISTRY_FILE" "$stage")"
    [ -n "$first_adjacent" ] && adjacent_label="$(_gen_events_input_with_label "$first_adjacent" "$stage")"
    [ -n "$first_shape" ] && shape_label="$(_gen_events_input_with_label "$first_shape" "$stage")"

    # ── decide each drifted file ───────────────────────────────────────────
    local f name status class why mine_lines="" pre_lines=""
    while IFS=$'\t' read -r status f; do
        [ -n "$f" ] || continue

        if [ "$f" = "index.ts" ]; then
            local b_names r_names moved hit rest_same=1
            b_names="$(_gen_events_barrel_names "$baseline/$f")"
            r_names="$(_gen_events_barrel_names "$regen/$f")"
            [ "$(_gen_events_barrel_rest "$baseline/$f")" = "$(_gen_events_barrel_rest "$regen/$f")" ] || rest_same=0
            moved="$(LC_ALL=C comm -3 <(printf '%s\n' "$b_names") <(printf '%s\n' "$r_names") | sed 's/^[[:space:]]*//;/^$/d')"
            hit=""
            while IFS= read -r name; do
                [ -n "$name" ] || continue
                _gen_events_name_in "$name" "$changed" && hit+="${hit:+, }$name"
            done <<< "$moved"
            if [ -n "$first_adjacent" ]; then
                mine_lines+="$f"$'\t'"a registry-adjacent input changed: $adjacent_label"$'\n'
            elif [ -n "$hit" ]; then
                mine_lines+="$f"$'\t'"export lines for names whose registration changed here: $hit; $registry_label"$'\n'
            elif [ "$rest_same" = "0" ]; then
                pre_lines+="$f"$'\t'"lines other than exports differ, and no input that writes them changed"$'\n'
            else
                pre_lines+="$f"$'\t'"$(printf '%s\n' "$moved" | _gen_events_count_lines) export line(s) whose registration did not change here"$'\n'
            fi
            continue
        fi

        name="$(printf '%s\n' "$f" | sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\)\.d\.ts$/\1/p')"
        if [ -z "$name" ]; then
            mine_lines+="$f"$'\t'"not a per-type binding, so no per-file rule applies"$'\n'
            continue
        fi

        case "$status" in
            A|D)
                if [ -n "$first_adjacent" ]; then
                    mine_lines+="$f"$'\t'"a registry-adjacent input changed: $adjacent_label"$'\n'
                elif _gen_events_name_in "$name" "$changed"; then
                    if [ "$status" = "A" ]; then
                        why="$name is registered here but not at the merge-base, and the bindings do not carry it: $registry_label"
                    else
                        why="$name is registered at the merge-base but not here, and the bindings still carry it: $registry_label"
                    fi
                    mine_lines+="$f"$'\t'"$why"$'\n'
                elif [ "$status" = "A" ]; then
                    pre_lines+="$f"$'\t'"$name is registered at the merge-base too; its binding was never committed"$'\n'
                else
                    pre_lines+="$f"$'\t'"$name is not registered at the merge-base either (a binding ahead of the runner)"$'\n'
                    FILE_ATTRIBUTION_AHEAD=$((FILE_ATTRIBUTION_AHEAD + 1))
                fi
                ;;
            M)
                # Pattern matching on the rows, not a pipe into a reader:
                # under the hook's pipefail, a reader that stops early can
                # SIGPIPE the writer on a large registry and end the hook with
                # no verdict. The parser emits one row per name.
                case $'\n'"$wt_rows"$'\n' in
                    *$'\n'"$name"$'\t'external$'\n'*) class=external ;;
                    *)                                  class=local ;;
                esac
                if [ -n "$first_shape" ]; then
                    mine_lines+="$f"$'\t'"an input that shapes every binding changed: $shape_label"$'\n'
                elif [ "$class" != "external" ]; then
                    mine_lines+="$f"$'\t'"$name is defined in this repo, and runner sources changed here (inputs listed below)"$'\n'
                else
                    pre_lines+="$f"$'\t'"$name is defined in the qontinui-schemas crates, and no manifest, lockfile, toolchain, build script or registry file changed here"$'\n'
                fi
                ;;
        esac
    done <<< "$drift"

    FILE_ATTRIBUTION_MINE="$(printf '%s' "$mine_lines" | sed '/^$/d')"
    FILE_ATTRIBUTION_PRE="$(printf '%s' "$pre_lines" | sed '/^$/d')"
    if [ -n "$FILE_ATTRIBUTION_MINE" ]; then
        FILE_ATTRIBUTION_STATE="mine"
    elif [ -n "$FILE_ATTRIBUTION_PRE" ]; then
        FILE_ATTRIBUTION_STATE="pre-existing"
    else
        FILE_ATTRIBUTION_REASON="the two trees do not differ, so there is nothing to attribute"
    fi
    return 0
}

# The codegen inputs that changed, each with its label, one per line. The
# pre-existing arm prints these. The failing arm uses gen_events_render_mine,
# whose notes about local state apply only there.
gen_events_render_inputs() {
    local indent="${1:-    }" stage p src
    stage="$(gen_events_stage)"
    while IFS=$'\t' read -r p src; do
        [ -n "$p" ] || continue
        printf '%s%s  (%s)\n' "$indent" "$p" "$(_gen_events_source_label "$stage" "$src")"
    done <<< "${ATTRIBUTION_TOUCHED_DETAIL:-}"
}

# The per-file verdict for the hook to print, one message line per stdout
# line. $1 is `mine` or `pre` (which list), $2 the indent.
gen_events_render_file_list() {
    local which="$1" indent="${2:-    }" list f why
    if [ "$which" = "mine" ]; then list="$FILE_ATTRIBUTION_MINE"; else list="$FILE_ATTRIBUTION_PRE"; fi
    while IFS=$'\t' read -r f why; do
        [ -n "$f" ] || continue
        printf '%s%s  (%s)\n' "$indent" "$f" "$why"
    done <<< "$list"
}
