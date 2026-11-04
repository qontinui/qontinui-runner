#!/usr/bin/env bash
# bot-owned-paths.sh — flag a PR that edits a BOT-OWNED path alongside other work.
#
# Plan: qontinui-dev-notes plans/2026-11-04-runner-generated-and-ledger-files-are-
# merge-neutral-and-land-behind-a-drain.md, Phase 0 and Design decision 4.
# Driven by .github/workflows/bot-owned-paths.yml; tested, with no network, by
# scripts/ci/bot-owned-paths-test.sh.
#
# ---------------------------------------------------------------------------
# WHY
# ---------------------------------------------------------------------------
# Two paths in this repo have a bot as their author of record:
#   .github/sibling-pins.conf       moved by sibling-pin-bump.yml (github-actions[bot])
#                                   and by schemas-pair-follow.yml
#   src-tauri/src/fleet_commands/** bytes copied from qontinui-claude-config, resynced
#                                   by the fleet-bundle-sync PRs (qontinui-dev[bot])
# Sweep 2's conflict census (2026-11-04) found 21 conflicting PRs on the pin file
# and 27 on the bundle, almost all of them feature PRs that carried a pin bump or a
# hand copy of a bundled file beside their code, then collided with the bot's next
# resync. A feature that needs a newer sibling lands a standalone pin bump first and
# declares the order with `coord:downstream-of=qontinui-runner#<pin PR>`.
#
# ---------------------------------------------------------------------------
# THE RULE (decision 4)
# ---------------------------------------------------------------------------
# A PR whose changed files include a bot-owned path FAILS unless one arm holds:
#   (a) its author login is github-actions[bot] or qontinui-dev[bot];
#   (b) EVERY changed file is .github/sibling-pins.conf, Cargo.lock or under
#       src-tauri/src/fleet_commands/ (a pin bump must move Cargo.lock with it);
#   (c) it carries `coord:downstream-of=[qontinui/]qontinui-schemas#N` — a declared
#       schemas-leads adaptation (form 2 of scripts/schemas-pair-follow.sh), where
#       the pin and the lock MUST ride with the code;
#   (d) it carries the label `ci:bot-owned-path-override`. The verdict then prints
#       an AUDIT line naming the label, who applied it, and the paths it covered.
# A PR touching no bot-owned path passes untouched. The arms are tried in the
# order above and the first that holds is named in the verdict.
#
# ---------------------------------------------------------------------------
# SUBCOMMANDS
# ---------------------------------------------------------------------------
#   check <author> <labels-file> <files-file> [<override-actor>]
#     Pure decision over local inputs, one label / one path per line. No network.
#   pr <owner/repo> <number>
#     Read the PR's author, labels and changed files (renames contribute both
#     names) through `gh api`, then run `check`.
#
# Exit codes: 0 = pass (a named arm, or no bot-owned path touched);
#             1 = fail (a bot-owned path and no arm holds);
#             3 = UNKNOWN (an API read failed, or the file list is truncated) —
#                 never read as a pass;
#             2 = usage.
set -euo pipefail

OVERRIDE_LABEL="ci:bot-owned-path-override"
BOT_AUTHORS=("github-actions[bot]" "qontinui-dev[bot]")
SCHEMAS_FORM2_RE='^coord:downstream-of=(qontinui/)?qontinui-schemas#[0-9]+$'

err() { printf 'bot-owned-paths: %s\n' "$*" >&2; }

# is_bot_owned PATH -> 0 when PATH is a bot-owned path.
is_bot_owned() {
  case "$1" in
    .github/sibling-pins.conf) return 0 ;;
    src-tauri/src/fleet_commands/*) return 0 ;;
  esac
  return 1
}

# is_bot_only_scope PATH -> 0 when PATH may ride in a bot-paths-only PR (arm b).
is_bot_only_scope() {
  is_bot_owned "$1" && return 0
  [ "$1" = "Cargo.lock" ]
}

summary() {
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then printf '%s\n' "$*" >>"$GITHUB_STEP_SUMMARY"; fi
}

verdict_pass() {
  echo "PASS: $1"
  summary "**bot-owned-paths: PASS** — $1"
  exit 0
}

cmd_check() {
  [ $# -ge 3 ] || { err "usage: check <author> <labels-file> <files-file> [<override-actor>]"; exit 2; }
  local author="$1" labels_file="$2" files_file="$3" actor="${4:-UNKNOWN}"
  [ -f "$labels_file" ] || { err "no labels file: $labels_file"; exit 2; }
  [ -f "$files_file" ] || { err "no files file: $files_file"; exit 2; }

  local owned=() others=() f
  while IFS= read -r f || [ -n "$f" ]; do
    [ -n "$f" ] || continue
    if is_bot_owned "$f"; then owned+=("$f"); fi
    if ! is_bot_only_scope "$f"; then others+=("$f"); fi
  done <"$files_file"

  echo "author: $author"
  echo "bot-owned paths touched: ${#owned[@]}"
  if [ "${#owned[@]}" -eq 0 ]; then
    verdict_pass "no bot-owned path touched"
  fi
  printf '  %s\n' "${owned[@]}"

  local b
  for b in "${BOT_AUTHORS[@]}"; do
    if [ "$author" = "$b" ]; then verdict_pass "arm (a): authored by $author"; fi
  done

  if [ "${#others[@]}" -eq 0 ]; then
    verdict_pass "arm (b): the diff touches only sibling-pins.conf, Cargo.lock and/or src-tauri/src/fleet_commands/**"
  fi

  local l
  while IFS= read -r l || [ -n "$l" ]; do
    if [[ "$l" =~ $SCHEMAS_FORM2_RE ]]; then
      verdict_pass "arm (c): declared schemas-leads adaptation ($l)"
    fi
  done <"$labels_file"

  if grep -qxF "$OVERRIDE_LABEL" "$labels_file"; then
    local audit
    audit="AUDIT: label $OVERRIDE_LABEL (applied by $actor) overrides bot-owned-paths for: ${owned[*]}"
    echo "$audit"
    echo "::warning title=bot-owned-paths override::$audit"
    summary "$audit"
    verdict_pass "arm (d): override label $OVERRIDE_LABEL"
  fi

  echo "FAIL: this PR edits a bot-owned path together with ${#others[@]} other file(s), and no arm holds:"
  printf '  other: %s\n' "${others[@]}"
  cat <<'EOF'
How to clear it — pick the one that is true:
  * You need a newer sibling: land the pin bump as its own PR (sibling-pins.conf +
    Cargo.lock only, or dispatch sibling-pin-bump.yml), drop the pin from this PR,
    and declare the order: coord:downstream-of=qontinui-runner#<pin PR>.
  * This is a schemas-leads adaptation: label it
    coord:downstream-of=qontinui-schemas#<N> (schemas-pair-follow.sh form 2).
  * You edited a bundled fleet command: change it in qontinui-claude-config and let
    the fleet-bundle-sync PR carry it here; drop the copy from this PR.
  * Genuinely neither: add the label ci:bot-owned-path-override. It is audited.
EOF
  echo "::error title=bot-owned-paths::a bot-owned path (${owned[*]}) is edited alongside other work; see the job log for how to clear it"
  summary "**bot-owned-paths: FAIL** — bot-owned: ${owned[*]}; other files: ${#others[@]}"
  exit 1
}

cmd_pr() {
  [ $# -eq 2 ] || { err "usage: pr <owner/repo> <number>"; exit 2; }
  local repo="$1" pr="$2"
  [[ "$pr" =~ ^[0-9]+$ ]] || { err "PR number must be numeric: $pr"; exit 2; }
  local tmp; tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT

  local meta author declared
  meta="$(gh api "repos/$repo/pulls/$pr")" || { err "UNKNOWN: could not read PR #$pr"; exit 3; }
  author="$(jq -r '.user.login // empty' <<<"$meta")"
  declared="$(jq -r '.changed_files // empty' <<<"$meta")"
  if [ -z "$author" ] || ! [[ "$declared" =~ ^[0-9]+$ ]]; then
    err "UNKNOWN: PR #$pr read carried no author or changed_files"
    exit 3
  fi

  # Labels read live, not from the event payload: a re-run after a label change
  # must see the label set as it is now.
  gh api --paginate "repos/$repo/issues/$pr/labels" --jq '.[].name' >"$tmp/labels" ||
    { err "UNKNOWN: could not read PR #$pr labels"; exit 3; }

  # Both names of a rename: moving a bot-owned file away is an edit to it.
  gh api --paginate "repos/$repo/pulls/$pr/files" \
    --jq '.[] | .filename, (.previous_filename // empty)' >"$tmp/files" ||
    { err "UNKNOWN: could not read PR #$pr changed files"; exit 3; }
  # GET /pulls/{n}/files silently truncates at 3000 entries (see ci-integrity.yml).
  local seen
  seen="$(gh api --paginate "repos/$repo/pulls/$pr/files" --jq '.[].filename' | wc -l | tr -d ' ')" ||
    { err "UNKNOWN: could not count PR #$pr changed files"; exit 3; }
  if [ "$seen" != "$declared" ]; then
    err "UNKNOWN: PR #$pr declares $declared changed files but the files API listed $seen (truncated?)"
    exit 3
  fi

  local actor="UNKNOWN"
  if grep -qxF "$OVERRIDE_LABEL" "$tmp/labels"; then
    actor="$(gh api --paginate "repos/$repo/issues/$pr/events" \
      --jq ".[] | select(.event == \"labeled\" and .label.name == \"$OVERRIDE_LABEL\") | .actor.login" |
      tail -n 1)" || actor="UNKNOWN"
    [ -n "$actor" ] || actor="UNKNOWN"
  fi

  cmd_check "$author" "$tmp/labels" "$tmp/files" "$actor"
}

case "${1:-}" in
  check) shift; cmd_check "$@" ;;
  pr) shift; cmd_pr "$@" ;;
  *) err "usage: $0 check <author> <labels-file> <files-file> [<override-actor>] | pr <owner/repo> <number>"; exit 2 ;;
esac
