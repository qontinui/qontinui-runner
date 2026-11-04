#!/usr/bin/env bash
# audit-ignores-test.sh — lint `.github/audit-ignores/` and the workflows that
# consume it (plan
# 2026-11-04-runner-generated-and-ledger-files-are-merge-neutral-and-land-behind-a-drain
# Phase 3c). Run by ci.yml's `security` job before `cargo audit`.
#
# Refuses:
#   * a file that is neither README.md nor a well-formed <RUSTSEC-YYYY-NNNN>.md;
#   * an ignore file with no line starting `Drop when` (its exit condition);
#   * any "review by" mention that is not exactly `REVIEW BY YYYY-MM-DD` with a
#     real date — the one form scripts/ci-audit-ignore-review.mjs reads, so a
#     misspelt date is refused here instead of silently never expiring;
#   * an inline `--ignore RUSTSEC-…` in any workflow: the directory is the only
#     place an ignore may live, or the per-file layout stops being the list;
#   * audit-ignore-flags.sh disagreeing with the directory.
#
# Usage: audit-ignores-test.sh [IGNORES_DIR [WORKFLOWS_DIR]]
# Exit: 0 clean; 1 one or more problems (each printed).
set -uo pipefail
export LC_ALL=C

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$here/../.."
dir="${1:-$root/.github/audit-ignores}"
workflows="${2:-$root/.github/workflows}"
fail=0
problem() { echo "audit-ignores-test: $*" >&2; fail=1; }

[ -d "$dir" ] || { problem "no directory $dir"; exit 1; }

is_real_date() {
  # GNU date normalises an impossible date (2026-02-30) into another day, so
  # compare the round trip rather than trusting the exit status alone.
  local d="$1" out
  out="$(date -u -d "$d" +%Y-%m-%d 2>/dev/null)" || return 1
  [ "$out" = "$d" ]
}

# The date grammar is scripts/ci-audit-ignore-review.mjs's `parseReviewDates`,
# spelled in the same PCRE (GNU grep -P), so a file this passes is one the
# nightly reader can read: a mention of "review by" in any spelling must be
# exactly `REVIEW BY YYYY-MM-DD`, a real date, and not wrapped across lines.
MENTION='\breview[\s_-]*by\b'
EXACT='\bREVIEW BY \d{4}-\d{2}-\d{2}(?![-\d])'

expected=()
# dotglob: a `.gitkeep` is refused here exactly as the .mjs reader refuses it.
shopt -s nullglob dotglob
for path in "$dir"/*; do
  name="$(basename "$path")"
  [ "$name" = "README.md" ] && continue
  if [ ! -f "$path" ] || ! [[ "$name" =~ ^RUSTSEC-[0-9]{4}-[0-9]{4}\.md$ ]]; then
    problem "$name: not a <RUSTSEC-YYYY-NNNN>.md file"
    continue
  fi
  expected+=("${name%.md}")
  grep -q '^Drop when' "$path" || problem "$name: no line starts with \`Drop when\` (the exit condition)"
  mapfile -t lines < <(sed 's/\r$//' "$path")
  for i in "${!lines[@]}"; do
    line="${lines[$i]}"
    lineno=$((i + 1))
    next="${lines[$((i + 1))]:-}"
    if grep -qiP '\breview\s*$' <<<"$line" && grep -qiP '^\s*by\b' <<<"$next"; then
      problem "$name:$lineno: a review date must be written \`REVIEW BY YYYY-MM-DD\` on one line"
      continue
    fi
    mentions="$(grep -oiP "$MENTION" <<<"$line" | grep -c .)"
    [ "$mentions" -eq 0 ] && continue
    exact="$(grep -oP "$EXACT" <<<"$line" | grep -c .)"
    if [ "$exact" -ne "$mentions" ]; then
      problem "$name:$lineno: a review date must be written \`REVIEW BY YYYY-MM-DD\`"
      continue
    fi
    for d in $(grep -oP "$EXACT" <<<"$line" | awk '{print $3}'); do
      is_real_date "$d" || problem "$name:$lineno: $d is not a real date"
    done
  done
done
shopt -u dotglob

if [ -d "$workflows" ]; then
  files=("$workflows"/*.yml "$workflows"/*.yaml)
  if [ "${#files[@]}" -gt 0 ]; then
    inline="$(grep -nE -- '--ignore[[:space:]=]+RUSTSEC-' "${files[@]}")"
    rc=$?
    if [ "$rc" -gt 1 ]; then
      problem "could not scan $workflows for inline ignores (grep exit $rc)"
    elif [ -n "$inline" ]; then
      problem "inline \`--ignore RUSTSEC-…\` in a workflow; add .github/audit-ignores/<id>.md instead:"
      echo "$inline" >&2
    fi
  fi
fi

if [ "$fail" -eq 0 ]; then
  got="$(bash "$here/audit-ignore-flags.sh" "$dir")" || problem "audit-ignore-flags.sh failed"
  want=""
  for id in "${expected[@]+"${expected[@]}"}"; do want="${want:+$want }--ignore $id"; done
  [ "$got" = "$want" ] || problem "audit-ignore-flags.sh printed '$got', expected '$want'"
fi

[ "$fail" -eq 0 ] && echo "audit-ignores-test: ok (${#expected[@]} ignores in $dir)"
exit "$fail"
