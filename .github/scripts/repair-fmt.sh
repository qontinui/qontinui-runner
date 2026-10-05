#!/usr/bin/env bash
# rustfmt EXACTLY the Rust files a PR changed — the `command` a `kind = "format"`
# `[[repair]]` in .qontinui/ci.toml names (plan
# 2026-09-24-coord-deterministic-ci-repair-lane §3 recipe 2).
#
# Usage (from the repo root, which is where coord-repair.yml runs it):
#   COORD_REPAIR_CHANGED_FILES=<file> bash .github/scripts/repair-fmt.sh [[!]path-prefix ...]
#
#   COORD_REPAIR_CHANGED_FILES  newline-separated repo-relative paths the PR
#                               changed (coord-repair-run.py writes it)
#   path-prefix                 optional: keep only files under one of these
#                               prefixes (e.g. `src-tauri/`), so a repair never
#                               reaches past what the failing CI step checks
#   !path-prefix                optional: drop files under this prefix even when
#                               an include matches (e.g. a separately-checked
#                               package nested inside an included one)
#
# NEVER repo-wide `cargo fmt`: the recipe's verifier refuses a patch that
# touches a file the PR did not change, and a repo-wide format on a tree with
# pre-existing drift would touch exactly those.
#
# Two things rustfmt does that this script has to bound:
#   1. Edition. A bare `rustfmt file.rs` formats as edition 2015 unless a
#      rustfmt.toml says otherwise, so each file is formatted with the edition
#      of the crate that owns it (nearest Cargo.toml; `edition.workspace = true`
#      reads the root manifest's [workspace.package]). A file whose edition
#      cannot be read is a refusal (exit 2), not a guess.
#   2. Child modules. rustfmt follows `mod foo;` declarations out of the file it
#      was given (`skip_children` is unstable), so formatting a changed lib.rs
#      would also rewrite every unchanged module under it. Any tracked file the
#      run modified that is NOT in the selected set is restored afterwards, so
#      the patch carries only PR-changed files.
#
# Exit: 0 formatted (or nothing to format), rustfmt's own code if it failed
# (e.g. a parse error), 2 a usage/edition refusal.
set -euo pipefail

die() { printf 'repair-fmt: %s\n' "$*" >&2; exit 2; }

: "${COORD_REPAIR_CHANGED_FILES:?COORD_REPAIR_CHANGED_FILES must name the changed-files list}"
[[ -f "$COORD_REPAIR_CHANGED_FILES" ]] || die "changed-files list not found: $COORD_REPAIR_CHANGED_FILES"
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || die "not inside a git work tree"
ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

declare -a includes=() excludes=()
for arg in "$@"; do
  [[ -n "$arg" && "$arg" != "!" ]] || die "empty path prefix"
  if [[ "$arg" == '!'* ]]; then excludes+=("${arg#!}"); else includes+=("$arg"); fi
done

in_scope() {
  local f="$1" p
  for p in "${excludes[@]}"; do
    [[ "$f" == "$p"* ]] && return 1
  done
  [[ ${#includes[@]} -eq 0 ]] && return 0
  for p in "${includes[@]}"; do
    [[ "$f" == "$p"* ]] && return 0
  done
  return 1
}

# The edition of the crate owning a repo-relative path: walk up to the nearest
# Cargo.toml carrying a [package] table.
workspace_edition() {
  awk '
    /^\[/ { in_ws = ($0 ~ /^\[workspace\.package\][[:space:]]*$/) }
    in_ws && /^[[:space:]]*edition[[:space:]]*=/ {
      if (match($0, /"[0-9]+"/)) { print substr($0, RSTART + 1, RLENGTH - 2); exit }
    }
  ' Cargo.toml 2>/dev/null
}

edition_of() {
  local dir manifest ed
  dir="$(dirname "$1")"
  while :; do
    manifest="$dir/Cargo.toml"
    [[ "$dir" == "." ]] && manifest="Cargo.toml"
    if [[ -f "$manifest" ]] && grep -q '^\[package\]' "$manifest"; then
      ed="$(awk '
        /^\[/ { in_pkg = ($0 ~ /^\[package\][[:space:]]*$/) }
        in_pkg && /^[[:space:]]*edition[[:space:]]*=/ {
          if (match($0, /"[0-9]+"/)) { print substr($0, RSTART + 1, RLENGTH - 2); exit }
        }
        in_pkg && /^[[:space:]]*edition[[:space:]]*\.[[:space:]]*workspace[[:space:]]*=[[:space:]]*true/ { print "workspace"; exit }
      ' "$manifest")"
      [[ "$ed" == "workspace" ]] && ed="$(workspace_edition)"
      printf '%s' "$ed"
      return 0
    fi
    [[ "$dir" == "." ]] && return 0
    dir="$(dirname "$dir")"
  done
}

declare -a selected=() file_editions=()
declare -A editions=()
while IFS= read -r f || [[ -n "$f" ]]; do
  [[ -z "$f" ]] && continue
  [[ "$f" == *.rs ]] || continue
  [[ -f "$f" ]] || continue          # deleted by the PR
  in_scope "$f" || continue
  ed="$(edition_of "$f")"
  [[ "$ed" =~ ^[0-9]{4}$ ]] || die "cannot read the crate edition for $f (no Cargo.toml [package] edition above it)"
  selected+=("$f")
  file_editions+=("$ed")
  editions[$ed]=1
done < "$COORD_REPAIR_CHANGED_FILES"

if [[ ${#selected[@]} -eq 0 ]]; then
  echo "repair-fmt: no changed .rs files in scope; nothing to format"
  exit 0
fi

echo "repair-fmt: formatting ${#selected[@]} changed .rs file(s)"
rc=0
for ed in "${!editions[@]}"; do
  files=()
  for i in "${!selected[@]}"; do
    # `./` so a path beginning with `-` can never read as an option.
    [[ "${file_editions[$i]}" == "$ed" ]] && files+=("./${selected[$i]}")
  done
  rustfmt --edition "$ed" "${files[@]}" || rc=$?
done

# Restore anything rustfmt reached through `mod` declarations that the PR did
# not change (see header, point 2).
declare -A keep=()
for f in "${selected[@]}"; do keep[$f]=1; done
declare -a restore=()
while IFS= read -r -d '' f; do
  [[ -n "${keep[$f]:-}" ]] || restore+=("$f")
done < <(git -c core.quotePath=false diff --name-only -z)
if [[ ${#restore[@]} -gt 0 ]]; then
  echo "repair-fmt: restoring ${#restore[@]} file(s) the PR did not change (reached via mod declarations)"
  git checkout -- "${restore[@]}"
fi

exit "$rc"
