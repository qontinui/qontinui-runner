#!/usr/bin/env bash
# audit-ignore-flags.sh — print the `cargo audit` `--ignore` flags, one per
# `.github/audit-ignores/<RUSTSEC-id>.md` file, on one line.
#
#   flags=$(bash scripts/ci/audit-ignore-flags.sh)   # in ci.yml's `security` job
#   cargo audit --file Cargo.lock $flags
#
# The directory is the list (plan
# 2026-11-04-runner-generated-and-ledger-files-are-merge-neutral-and-land-behind-a-drain
# Phase 3c): one file per advisory, so two PRs adding different ignores never
# touch the same line. README.md is not an ignore.
#
# Usage: audit-ignore-flags.sh [DIR]   (default: .github/audit-ignores at the repo root)
#
# Exit codes: 0 printed (possibly nothing: no ignores is a valid answer);
# 1 a file in DIR is not README.md and not a well-formed <RUSTSEC-YYYY-NNNN>.md,
# or DIR is missing. It refuses rather than skips, because a skipped file is an
# ignore the job silently stops applying (or, worse, one nobody reviews).
set -euo pipefail
export LC_ALL=C  # a stable glob order

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dir="${1:-$here/../../.github/audit-ignores}"
[ -d "$dir" ] || { echo "audit-ignore-flags: no directory $dir" >&2; exit 1; }

flags=()
# dotglob: a `.gitkeep` is refused, as the review script refuses it.
shopt -s nullglob dotglob
for path in "$dir"/*; do
  name="$(basename "$path")"
  [ "$name" = "README.md" ] && continue
  if [ ! -f "$path" ] || ! [[ "$name" =~ ^RUSTSEC-[0-9]{4}-[0-9]{4}\.md$ ]]; then
    echo "audit-ignore-flags: $path is not a <RUSTSEC-YYYY-NNNN>.md file" >&2
    exit 1
  fi
  flags+=(--ignore "${name%.md}")
done
echo "${flags[*]+"${flags[*]}"}"
