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
# Bot-owned paths have an automation as their author of record:
#   .github/sibling-pins.conf        moved by sibling-pin-bump.yml (whose PRs are
#                                    opened under the CLORINDE_AUTOCOMMIT_TOKEN
#                                    user, so they pass on arm (b), not arm (a))
#                                    and by schemas-pair-follow.yml
#   src-tauri/src/fleet_commands/**  bytes copied from qontinui-claude-config and
#   src-tauri/src/fleet_skills/**    resynced by the fleet-bundle-sync PRs
#   src-tauri/src/fleet_agents/**    (author qontinui-dev[bot], e.g. #2549; the three
#                                    directories are qontinui-claude-config
#                                    scripts/fleet-bundle-sync.sh's PAIRS)
# Sweep 2's conflict census (2026-11-04) found 21 conflicting PRs on the pin file
# and 27 on fleet_commands/**, almost all of them feature PRs that carried a pin
# bump or a hand copy of a bundled file beside their code, then collided with the
# bot's next resync. A feature that needs a newer sibling lands a standalone pin
# bump first and declares the order with `coord:downstream-of=qontinui-runner#<pin PR>`.
#
# ---------------------------------------------------------------------------
# THE RULE (decision 4)
# ---------------------------------------------------------------------------
# A PR whose changed files include a bot-owned path FAILS unless one arm holds:
#   (a) its author login is github-actions[bot] or qontinui-dev[bot];
#   (b) EVERY changed file is a bot-owned path or the root Cargo.lock (a pin bump
#       must move Cargo.lock with it);
#   (c) it carries `coord:downstream-of=[qontinui/]qontinui-schemas#N`, a declared
#       schemas-leads adaptation (form 2 of scripts/schemas-pair-follow.sh) where
#       the pin and the lock MUST ride with the code — OR its label history shows
#       that label last REMOVED BY AUTOMATION: coord's App strips it when the
#       partner lands, and schemas-pair-follow.sh deletes a remaining copy (as
#       github-actions[bot]) before it pushes the pin in. So the push that brings
#       the pin in is exactly the run that no longer sees the label. A label a
#       person removed is NOT a declaration. That is the intent of
#       schemas-pair-follow.sh's stripped_by_coord, but the rule here is stricter:
#       the LAST event per label decides, and two automations count (coord's App
#       and github-actions[bot]). stripped_by_coord accepts a coord strip anywhere
#       in the history, so a strip, a re-add and then a hand removal satisfies it
#       and fails here. One residual: if the events API has not yet shown
#       pair-follow's delete when its push fires `synchronize`, that run reds. It
#       fails safe, and the next label event or push re-decides. The verdict
#       prints an AUDIT line naming the label and everyone who applied it;
#   (d) it carries the label `ci:bot-owned-path-override`. The verdict prints an
#       AUDIT line naming the label, who applied it, and the paths it covered.
# A PR touching no bot-owned path passes untouched. The arms are tried in the
# order above and the first that holds is named in the verdict.
#
# ---------------------------------------------------------------------------
# SUBCOMMANDS
# ---------------------------------------------------------------------------
#   check <author> <labels-file> <files-file> [<events-file>]
#     Pure decision over local inputs, one label / one path per line. The optional
#     events file holds the PR's label history, one `<event>\t<label>\t<actor>`
#     line per labeled/unlabeled event, oldest first. No network.
#   pr <owner/repo> <number>
#     Read the PR's author, labels, label history and changed files (renames
#     contribute both names) through `gh api`, then run `check`.
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

# Workflow-command data must have %, CR and LF escaped, or a crafted file name
# can end the command early and inject another.
wf_escape() {
  local s="$1"
  s="${s//'%'/%25}"
  s="${s//$'\r'/%0D}"
  s="${s//$'\n'/%0A}"
  printf '%s' "$s"
}

# is_bot_owned PATH -> 0 when PATH is a bot-owned path.
is_bot_owned() {
  case "$1" in
    .github/sibling-pins.conf) return 0 ;;
    src-tauri/src/fleet_commands/*) return 0 ;;
    src-tauri/src/fleet_skills/*) return 0 ;;
    src-tauri/src/fleet_agents/*) return 0 ;;
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

audit() {
  echo "$1"
  echo "::warning title=bot-owned-paths audit::$(wf_escape "$1")"
  summary "$1"
}

# Who may remove a form-2 label without withdrawing the declaration (arm c):
# coord's App, and schemas-pair-follow.sh's own GITHUB_TOKEN delete.
STRIP_ACTOR_COORD="qontinui-merge-orchestrator[bot]"
STRIP_ACTOR_ACTIONS="github-actions[bot]"

# applier EVENTS-FILE LABEL -> every distinct actor of a `labeled` event for LABEL,
# in first-seen order and comma-joined, or UNKNOWN. All of them, because a label
# re-added by automation (pair-follow's restore) would otherwise hide the person.
applier() {
  local events="$1" label="$2" a=""
  if [ -n "$events" ] && [ -f "$events" ]; then
    a="$(awk -F'\t' -v l="$label" '$1 == "labeled" && $2 == l && !seen[$3]++ { out = out (out == "" ? "" : ",") $3 } END { print out }' "$events")"
  fi
  printf '%s' "${a:-UNKNOWN}"
}

cmd_check() {
  [ $# -ge 3 ] || { err "usage: check <author> <labels-file> <files-file> [<events-file>]"; exit 2; }
  local author="$1" labels_file="$2" files_file="$3" events_file="${4:-}"
  [ -f "$labels_file" ] || { err "no labels file: $labels_file"; exit 2; }
  [ -f "$files_file" ] || { err "no files file: $files_file"; exit 2; }
  if [ -n "$events_file" ] && [ ! -f "$events_file" ]; then err "no events file: $events_file"; exit 2; fi

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
    verdict_pass "arm (b): the diff touches only bot-owned paths and/or Cargo.lock"
  fi

  # Arm (c): a form-2 label now present first, else one the label history shows.
  local l state=""
  local form2=""
  while IFS= read -r l || [ -n "$l" ]; do
    if [[ "$l" =~ $SCHEMAS_FORM2_RE ]]; then form2="$l"; state="present"; break; fi
  done <"$labels_file"
  if [ -z "$form2" ] && [ -n "$events_file" ]; then
    # The LAST event per form-2 label decides; accept one last unlabeled by automation.
    form2="$(awk -F'\t' -v re="$SCHEMAS_FORM2_RE" -v s1="$STRIP_ACTOR_COORD" -v s2="$STRIP_ACTOR_ACTIONS" '
      $2 ~ re { ev[$2] = $1; who[$2] = $3; if (!($2 in ord)) { ord[$2] = ++k; name[k] = $2 } }
      END { for (i = 1; i <= k; i++) { l = name[i]; if (ev[l] == "unlabeled" && (who[l] == s1 || who[l] == s2)) last = l } print last }
    ' "$events_file")"
    [ -z "$form2" ] || state="since removed by automation"
  fi
  if [ -n "$form2" ]; then
    audit "AUDIT: arm (c) label $form2 ($state; applied by $(applier "$events_file" "$form2")) admits bot-owned paths: ${owned[*]}"
    verdict_pass "arm (c): declared schemas-leads adaptation ($form2, $state)"
  fi

  if grep -qxF "$OVERRIDE_LABEL" "$labels_file"; then
    audit "AUDIT: label $OVERRIDE_LABEL (applied by $(applier "$events_file" "$OVERRIDE_LABEL")) overrides bot-owned-paths for: ${owned[*]}"
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
  * You edited a bundled fleet command, skill or agent: change it in
    qontinui-claude-config and let the fleet-bundle-sync PR carry it here; drop
    the copy from this PR.
  * Genuinely neither (e.g. a pin REMOVAL that must also drop its checkout step
    from ci.yml): add the label ci:bot-owned-path-override. It is audited.
EOF
  echo "::error title=bot-owned-paths::$(wf_escape "a bot-owned path (${owned[*]}) is edited alongside other work; see the job log for how to clear it")"
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
  gh api --paginate "repos/$repo/issues/$pr/labels?per_page=100" --jq '.[].name' >"$tmp/labels" ||
    { err "UNKNOWN: could not read PR #$pr labels"; exit 3; }

  gh api --paginate "repos/$repo/issues/$pr/events?per_page=100" \
    --jq '.[] | select(.event == "labeled" or .event == "unlabeled") | [.event, .label.name, (.actor.login // "UNKNOWN")] | @tsv' \
    >"$tmp/events" || { err "UNKNOWN: could not read PR #$pr label history"; exit 3; }

  # ONE walk of the file list, so the names and their count come from the same
  # read. `changed_files` comes from the earlier PR read: a head that moves
  # between the two is caught only when the count changes, and a mismatch fails
  # safe (exit 3). One JSON array per entry: [filename, previous_filename].
  gh api --paginate "repos/$repo/pulls/$pr/files?per_page=100" \
    --jq '.[] | [.filename, (.previous_filename // "")] | @json' >"$tmp/files.jsonl" ||
    { err "UNKNOWN: could not read PR #$pr changed files"; exit 3; }
  # GET /pulls/{n}/files silently truncates at 3000 entries (see ci-integrity.yml).
  # KNOWN LIMIT: a PR over 3000 files is therefore always UNKNOWN, and no arm,
  # not even the override, can clear it. Split the PR.
  local seen
  seen="$(grep -c . "$tmp/files.jsonl" || true)"
  if [ "$seen" != "$declared" ]; then
    err "UNKNOWN: PR #$pr declares $declared changed files but the files API listed $seen (truncated, or the head moved)"
    exit 3
  fi
  # The decision reads one path per line, so a name carrying CR or LF would be
  # split into fake entries (and could satisfy arm (b)). Refuse it as UNKNOWN.
  # -s: one verdict over every entry (-e alone reads only the LAST output).
  if ! jq -s -e 'all(.[][]; test("[\r\n]") | not)' "$tmp/files.jsonl" >/dev/null; then
    err "UNKNOWN: PR #$pr has a changed file whose name contains CR or LF"
    exit 3
  fi
  # Both names of a rename: moving a bot-owned file away is an edit to it.
  jq -r '.[] | select(. != "")' "$tmp/files.jsonl" >"$tmp/files" ||
    { err "UNKNOWN: could not parse PR #$pr changed files"; exit 3; }

  cmd_check "$author" "$tmp/labels" "$tmp/files" "$tmp/events"
}

case "${1:-}" in
  check) shift; cmd_check "$@" ;;
  pr) shift; cmd_pr "$@" ;;
  *) err "usage: $0 check <author> <labels-file> <files-file> [<events-file>] | pr <owner/repo> <number>"; exit 2 ;;
esac
