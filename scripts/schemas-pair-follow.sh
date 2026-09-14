#!/usr/bin/env bash
# schemas-pair-follow.sh — follow a LANDED qontinui-schemas partner into the runner
# PR that adapts to it, by moving that PR's schemas pin (and Cargo.lock) to the
# partner's land commit.
#
# Plan: qontinui-dev-notes plans/2026-08-31-schemas-releases-strand-consumer-cargo-locks.md §7.7
# (Phase 3.0). Driven by .github/workflows/schemas-pair-follow.yml; tested, with
# no network, by scripts/tests/schemas-pair-follow/run.sh.
#
# ---------------------------------------------------------------------------
# WHY
# ---------------------------------------------------------------------------
# qontinui-schemas is pinned in .github/sibling-pins.conf. A schemas-leads
# adaptation pair is a runner PR M adapting to a breaking schemas PR N. It is
# declared either as `coord:downstream-of=[qontinui/]qontinui-schemas#N` on M
# (form 2) or as `coord:upstream-of=[qontinui/]qontinui-runner#M` on N (form 1).
# Once N lands, three things line up against M:
#   - coord strips M's label;
#   - checkout-sibling refuses a declaration that is not open;
#   - a merge-candidate push carries no PR number, so it reads no declaration
#     at all.
# So M's rerun and its candidate build both compile the OLD pin and stay red,
# until someone moves the pin and Cargo.lock inside M. This script makes that
# one move, and refuses whenever it cannot prove the move is safe.
#
# ---------------------------------------------------------------------------
# SUBCOMMANDS
# ---------------------------------------------------------------------------
#   scan
#       Print one "M N" candidate per line, from both declaration forms.
#   decide <M> <N>
#       Print ONE JSON object whose action is FOLLOW / SKIP / UNKNOWN. Always
#       exits 0; the caller counts UNKNOWNs and fails red on them.
#   merge-and-pin <runner-dir> <meta.json>
#       UNPRIVILEGED compute job. Merges runner main into M's head, then moves
#       the pin to A unless the merge already contains it. Prints MODE= and
#       REASON= lines.
#   lock-and-commit <runner-dir> <meta.json> <out-dir> <MODE>
#       UNPRIVILEGED compute job. Refreshes Cargo.lock resolve-only, asserts
#       the file scope, commits, and writes <out-dir>/pair-follow.bundle and
#       <out-dir>/meta.json.
#   push <out-dir> <scratch-dir>
#       PRIVILEGED push job, running main's code only. Re-verifies the bundle
#       structure and scope, re-reads M's head, removes a still-present form-2
#       label, pushes with --force-with-lease on the head it built on, and
#       comments once. Prints RESULT=.
#
# Exit codes: 0 = a named outcome (FOLLOW / MERGE_ONLY / SKIP / ABORT / PUSHED);
#             3 = UNKNOWN (the lane must go red); 2 = usage error.
#
# ENV
#   SPF_FIXTURES             fixture root (gh/<name>, coord/<name>, writes.log); no network
#   SPF_KILL_SWITCH          the repository variable SCHEMAS_PAIR_FOLLOW; 'off' disables
#   COORD_CLAIMS_READ_TOKEN  optional bearer for coord claim reads (see plan §7.7)
#   SPF_COORD_URL            default https://coord.qontinui.io
#   SPF_RUNNER_REPO          default qontinui/qontinui-runner
#   SPF_LOCK_CMD             lock refresh (default: cargo metadata, then --locked)
#   SPF_PUSH_TOKEN           PAT for the push (CLORINDE_AUTOCOMMIT_TOKEN)
#   SPF_PUSH_URL / SPF_FETCH_URL  remotes (default https://github.com/<runner>.git)
#   SPF_FETCH_FILTER         clone filter for the push job's verify clone (default blob:none; '' = none)
set -euo pipefail

RUNNER="${SPF_RUNNER_REPO:-qontinui/qontinui-runner}"
RUNNER_NAME="${RUNNER#*/}"
SCHEMAS="qontinui/qontinui-schemas"
CONF=".github/sibling-pins.conf"
COORD="${SPF_COORD_URL:-https://coord.qontinui.io}"
# The coord GitHub App that posts the land announcement (render_ff_land_comment).
LAND_BOT="qontinui-merge-orchestrator[bot]"
TRAILER_KEY="Schemas-Pair-Follow"
BOT_NAME="github-actions[bot]"
BOT_EMAIL="41898283+github-actions[bot]@users.noreply.github.com"

err() { printf 'schemas-pair-follow: %s\n' "$*" >&2; }
fxname() { printf '%s' "$1" | sed 's#[^A-Za-z0-9._-]#_#g'; }
is_sha() { [[ "$1" =~ ^[0-9a-f]{40}$ ]]; }

# --- GitHub reads ----------------------------------------------------------
# gh_get PATH -> body on stdout. rc 0 = ok, 44 = HTTP 404, 1 = any other failure.
gh_get() {
  local path="$1"
  if [ -n "${SPF_FIXTURES:-}" ]; then
    local f; f="$SPF_FIXTURES/gh/$(fxname "$path")"
    if [ ! -f "$f" ]; then err "no fixture for gh $path"; return 1; fi
    case "$(head -c 7 "$f")" in
      __404__) return 44 ;;
      __ERR__) err "fixture error for gh $path"; return 1 ;;
    esac
    cat "$f"
    return 0
  fi
  local errf out
  errf="$(mktemp)"
  if out="$(gh api "$path" 2>"$errf")"; then
    rm -f "$errf"
    printf '%s' "$out"
    return 0
  fi
  if grep -q 'HTTP 404' "$errf"; then rm -f "$errf"; return 44; fi
  err "gh api $path failed: $(tr '\n' ' ' <"$errf")"
  rm -f "$errf"
  return 1
}

# gh_write METHOD PATH [gh api field args...]
gh_write() {
  local method="$1" path="$2"
  shift 2
  if [ -n "${SPF_FIXTURES:-}" ]; then
    printf '%s %s %s\n' "$method" "$path" "$*" >>"$SPF_FIXTURES/writes.log"
    return 0
  fi
  gh api -X "$method" "$path" "$@" >/dev/null
}

# --- coord claim reads -----------------------------------------------------
# claim_state KIND KEY -> prints "free" or "held". rc 3 = UNKNOWN (never "free").
# Mirrors qontinui-coord pr_merge/behind_handler.rs `pr_holds_live_claim`, plus
# the RepoBranch mutex (plan §7.7).
claim_state() {
  local kind="$1" key="$2" status body
  if [ -n "${SPF_FIXTURES:-}" ]; then
    local f; f="$SPF_FIXTURES/coord/$(fxname "$kind:$key")"
    status="200"; [ -f "$f.status" ] && status="$(cat "$f.status")"
    body='{"holder":null,"last_release":null}'; [ -f "$f" ] && body="$(cat "$f")"
  else
    local enc tmp
    enc="$(jq -rn --arg k "$key" '$k|@uri')"
    tmp="$(mktemp)"
    # The bearer travels in a curl config on a pipe, never on argv.
    status="$(curl -sS --max-time 30 --retry 2 -o "$tmp" -w '%{http_code}' \
      -K <(if [ -n "${COORD_CLAIMS_READ_TOKEN:-}" ]; then printf 'header = "Authorization: Bearer %s"\n' "$COORD_CLAIMS_READ_TOKEN"; fi) \
      "$COORD/coord/claims/by-resource?kind=$kind&key=$enc&include_last_release=true")" || status="000"
    body="$(cat "$tmp")"
    rm -f "$tmp"
  fi
  case "$status" in
    200) ;;
    401 | 403)
      err "claim read $kind/$key -> HTTP $status: coord requires an authenticated claims read. Provision the COORD_CLAIMS_READ_TOKEN secret. UNKNOWN, not 'no claim'."
      return 3 ;;
    *)
      err "claim read $kind/$key -> HTTP $status. UNKNOWN, not 'no claim'."
      return 3 ;;
  esac
  local holder
  # No `-e`: a free claim IS `"holder": null`, and `jq -e` exits 1 on a null result.
  # A wrong shape still fails, through error().
  if ! holder="$(printf '%s' "$body" | jq -c 'if type == "object" and has("holder") then .holder else error("shape") end' 2>/dev/null)"; then
    err "claim read $kind/$key: unparseable body. UNKNOWN, not 'no claim'."
    return 3
  fi
  if [ "$holder" = "null" ]; then echo free; else echo held; fi
}

# --- pins, landing, containment --------------------------------------------
pin_of() { tr -d '\r' | awk -v s="$SCHEMAS" '{ sub(/#.*/, "") } $1 == s && NF == 2 { print $2 }'; }

# conf_at REF -> the runner conf text at REF. rc 44 when the file is absent.
conf_at() {
  local json rc=0
  json="$(gh_get "repos/$RUNNER/contents/$CONF?ref=$1")" || rc=$?
  [ "$rc" -eq 0 ] || return "$rc"
  printf '%s' "$json" | jq -r '.content' | base64 -d
}

# contains A PIN -> rc 0 when PIN contains A, 1 when not, 3 UNKNOWN.
# compare/<base>...<head> reports HEAD relative to BASE, so PIN (head) containing
# A (base) reads `ahead` or `identical`.
contains() {
  local a="$1" pin="$2" st
  [ "$a" = "$pin" ] && return 0
  if ! st="$(gh_get "repos/$SCHEMAS/compare/$a...$pin" | jq -r '.status // empty')"; then
    err "compare $a...$pin unreadable"
    return 3
  fi
  case "$st" in
    ahead | identical) return 0 ;;
    behind | diverged) return 1 ;;
    *) err "compare $a...$pin returned '$st'"; return 3 ;;
  esac
}

# landed_state N_JSON -> landed | not-on-main | open | abandoned
landed_state() {
  jq -r '
    if (.base.ref // "") != "main" then "not-on-main"
    elif (.merged_at != null) or ([.labels[]?.name] | index("coord:landed") != null) then "landed"
    elif .state == "open" then "open"
    else "abandoned" end' <<<"$1"
}

# land_commit N_JSON -> the full sha of N's land commit A, verified on schemas main. rc 3 UNKNOWN.
land_commit() {
  local nj="$1" n sha="" short closed st
  n="$(jq -r .number <<<"$nj")"
  if [ "$(jq -r '.merged_at // empty' <<<"$nj")" != "" ]; then
    sha="$(jq -r '.merge_commit_sha // empty' <<<"$nj")"
  else
    local comments
    comments="$(gh_get "repos/$SCHEMAS/issues/$n/comments?per_page=100")" || return 3
    short="$(jq -r --arg bot "$LAND_BOT" '
      [.[] | select(.user.login == $bot) | .body
       | capture("Landed on `main` by coord(\\*\\*)? as `(?<s>[0-9a-f]{7,40})`")? | .s] | last // empty' <<<"$comments")"
    if [ -n "$short" ]; then
      sha="$(gh_get "repos/$SCHEMAS/commits/$short" | jq -r '.sha // empty')" || return 3
    else
      closed="$(jq -r '.closed_at // empty' <<<"$nj")"
      [ -n "$closed" ] || { err "#$n has no merge commit, no coord land comment and no closed_at"; return 3; }
      sha="$(gh_get "repos/$SCHEMAS/commits?sha=main&until=$closed&per_page=1" | jq -r '.[0].sha // empty')" || return 3
    fi
  fi
  is_sha "$sha" || { err "land commit of $SCHEMAS#$n is unresolvable"; return 3; }
  if ! st="$(gh_get "repos/$SCHEMAS/compare/$sha...main" | jq -r '.status // empty')"; then
    err "compare $sha...main unreadable"
    return 3
  fi
  case "$st" in
    ahead | identical) printf '%s\n' "$sha" ;;
    *) err "land commit $sha of #$n is '$st' relative to schemas main — not on main"; return 3 ;;
  esac
}

label_names() { jq -r '[.labels[]?.name] | .[]' <<<"$1"; }
has_label_re() { label_names "$1" | grep -Eq "$2"; }

# --- decide ----------------------------------------------------------------
cmd_decide() {
  local m="$1" n="$2"
  local action="" reason="" a="" head_sha="" head_ref="" main_sha="" mode="" label_present=false
  emit() {
    jq -cn --argjson pr "$m" --argjson partner "$n" --arg action "$action" --arg reason "$reason" \
      --arg a "$a" --arg head_sha "$head_sha" --arg head_ref "$head_ref" --arg main_sha "$main_sha" \
      --arg mode "$mode" --argjson label_present "$label_present" \
      '{pr:$pr, partner:$partner, action:$action, reason:$reason, a:$a, head_sha:$head_sha,
        head_ref:$head_ref, main_sha:$main_sha, mode:$mode, label_present:$label_present}'
  }
  skip() { action=SKIP; reason="$1"; emit; }
  unknown() { action=UNKNOWN; reason="$1"; emit; }

  [[ "$m" =~ ^[0-9]+$ && "$n" =~ ^[0-9]+$ ]] || { err "decide: M and N must be numbers"; exit 2; }
  if [ "${SPF_KILL_SWITCH:-}" = "off" ]; then skip kill-switch; return; fi

  local nj mj
  nj="$(gh_get "repos/$SCHEMAS/pulls/$n")" || { unknown "read $SCHEMAS#$n failed"; return; }
  mj="$(gh_get "repos/$RUNNER/pulls/$m")" || { unknown "read $RUNNER#$m failed"; return; }

  # Declaration: which form, if any, pairs M with N with schemas LEADING.
  local re_f1="^coord:upstream-of=(qontinui/)?$RUNNER_NAME#$m\$"
  local re_f2="^coord:downstream-of=(qontinui/)?qontinui-schemas#$n\$"
  local re_t_m="^coord:upstream-of=(qontinui/)?qontinui-schemas#$n\$"
  local re_t_n="^coord:downstream-of=(qontinui/)?$RUNNER_NAME#$m\$"
  local leads=false
  has_label_re "$nj" "$re_f1" && leads=true
  if has_label_re "$mj" "$re_f2"; then leads=true; label_present=true; fi
  if [ "$leads" != true ]; then
    local events
    events="$(gh_get "repos/$RUNNER/issues/$m/events?per_page=100")" || { unknown "read $RUNNER#$m events failed"; return; }
    if jq -r '.[] | select(.event == "labeled") | .label.name' <<<"$events" | grep -Eq "$re_f2"; then leads=true; fi
  fi
  if [ "$leads" != true ]; then
    if has_label_re "$mj" "$re_t_m" || has_label_re "$nj" "$re_t_n"; then skip trailing-declaration; else skip no-declaration; fi
    return
  fi

  case "$(landed_state "$nj")" in
    landed) ;;
    not-on-main) skip partner-not-on-main; return ;;
    open) skip partner-not-landed; return ;;
    *) skip partner-abandoned; return ;;
  esac
  [ "$(jq -r .state <<<"$mj")" = "open" ] || { skip pr-not-open; return; }
  [ "$(jq -r '.head.repo.full_name // ""' <<<"$mj")" = "$RUNNER" ] || { skip fork-pr; return; }

  main_sha="$(gh_get "repos/$RUNNER/commits/main" | jq -r '.sha // empty')" || { unknown "read runner main failed"; return; }
  is_sha "$main_sha" || { unknown "runner main sha unresolvable"; return; }
  local main_conf rc=0 main_pin
  main_conf="$(conf_at "$main_sha")" || rc=$?
  if [ "$rc" -eq 44 ]; then skip no-pin; return; fi
  [ "$rc" -eq 0 ] || { unknown "read runner main conf failed"; return; }
  main_pin="$(printf '%s' "$main_conf" | pin_of)"
  [ -n "$main_pin" ] || { skip no-pin; return; }

  a="$(land_commit "$nj")" || { a=""; unknown "land-commit-unresolvable"; return; }

  head_sha="$(jq -r '.head.sha' <<<"$mj")"
  head_ref="$(jq -r '.head.ref' <<<"$mj")"
  local head_conf head_pin="" c
  rc=0
  head_conf="$(conf_at "$head_sha")" || rc=$?
  if [ "$rc" -eq 0 ]; then
    head_pin="$(printf '%s' "$head_conf" | pin_of)"
  elif [ "$rc" -ne 44 ]; then
    unknown "read PR head conf failed"
    return
  fi
  if [ -n "$head_pin" ]; then
    c=0; contains "$a" "$head_pin" || c=$?
    case "$c" in
      0) skip contained; return ;;
      3) unknown "containment of the PR head pin unknown"; return ;;
    esac
  fi
  c=0; contains "$a" "$main_pin" || c=$?
  case "$c" in
    0) mode=MERGE_ONLY ;;
    1) mode=FOLLOW ;;
    *) unknown "containment of runner main's pin unknown"; return ;;
  esac

  local kind key st
  for kind_key in "branch_name|$head_ref" "ci_wait|$RUNNER_NAME#$m" "ci_wait|$RUNNER#$m" "repo_branch|$RUNNER_NAME:$head_ref"; do
    kind="${kind_key%%|*}"
    key="${kind_key#*|}"
    if ! st="$(claim_state "$kind" "$key")"; then unknown "claim read $kind unknown"; return; fi
    if [ "$st" = "held" ]; then skip "live-claim:$kind"; return; fi
  done

  action=FOLLOW
  reason="$SCHEMAS#$n landed as ${a:0:9}"
  emit
}

# --- scan ------------------------------------------------------------------
cmd_scan() {
  if [ "${SPF_KILL_SWITCH:-}" = "off" ]; then err "kill switch SCHEMAS_PAIR_FOLLOW=off"; return 0; fi
  local open page=1 all="[]" chunk
  while :; do
    chunk="$(gh_get "repos/$RUNNER/pulls?state=open&per_page=100&page=$page")" || { err "list open runner PRs failed"; exit 3; }
    all="$(jq -c --argjson c "$chunk" '. + $c' <<<"$all")"
    [ "$(jq length <<<"$chunk")" -lt 100 ] && break
    page=$((page + 1))
  done
  open="$all"
  [ "$(jq length <<<"$open")" -gt 0 ] || return 0
  local oldest numbers
  oldest="$(jq -r '[.[].created_at] | min' <<<"$open")"
  numbers="$(jq -r '.[].number' <<<"$open")"

  {
    # Form 2: open runner PRs still labelled (a strip not reached yet, or a missed event).
    jq -r '.[] | .number as $m | .labels[]?.name
      | capture("^coord:downstream-of=(qontinui/)?qontinui-schemas#(?<n>[0-9]+)$")? | "\($m) \(.n)"' <<<"$open"

    # Form 1: landed schemas PRs naming an open runner PR. The window reaches back
    # to the oldest open runner PR, not a fixed number of days.
    page=1
    while :; do
      chunk="$(gh_get "repos/$SCHEMAS/pulls?state=closed&sort=updated&direction=desc&per_page=100&page=$page")" || { err "list closed schemas PRs failed"; exit 3; }
      jq -r --arg r "$RUNNER_NAME" --arg oldest "$oldest" '
        .[] | select(.updated_at >= $oldest) | .number as $n | .labels[]?.name
        | capture("^coord:upstream-of=(qontinui/)?" + $r + "#(?<m>[0-9]+)$")? | "\(.m) \($n)"' <<<"$chunk" |
        while read -r cm cn; do
          if grep -qx "$cm" <<<"$numbers"; then echo "$cm $cn"; fi
        done
      if [ "$(jq length <<<"$chunk")" -lt 100 ]; then break; fi
      if [ "$(jq -r --arg oldest "$oldest" '[.[] | select(.updated_at < $oldest)] | length' <<<"$chunk")" -gt 0 ]; then break; fi
      page=$((page + 1))
    done
  } | LC_ALL=C sort -u
}

# --- compute: merge-and-pin -------------------------------------------------
meta_get() { jq -r ".$2 // empty" "$1"; }

rewrite_pin() { # conf new
  local conf="$1" new="$2" tmp
  tmp="$(mktemp)"
  awk -v s="$SCHEMAS" -v new="$new" '
    { line = $0; code = line; sub(/#.*/, "", code); split(code, f, " ") }
    f[1] == s && f[2] ~ /^[0-9a-f]{40}$/ { sub(f[2], new, line) }
    { print line }' "$conf" >"$tmp"
  cat "$tmp" >"$conf"
  rm -f "$tmp"
}

cmd_merge_and_pin() {
  local dir="$1" meta="$2" head main a n m pin c
  head="$(meta_get "$meta" head_sha)"; main="$(meta_get "$meta" main_sha)"
  a="$(meta_get "$meta" a)"; n="$(meta_get "$meta" partner)"; m="$(meta_get "$meta" pr)"
  is_sha "$head" && is_sha "$main" && is_sha "$a" || { err "merge-and-pin: bad meta"; exit 3; }
  cd "$dir"
  [ "$(git rev-parse HEAD)" = "$head" ] || { err "checkout is not at the decided head $head"; exit 3; }
  if ! git -c user.name="$BOT_NAME" -c user.email="$BOT_EMAIL" merge --no-edit --no-ff -q "$main" \
    -m "Merge runner main into #$m for the qontinui-schemas#$n pair-follow" >/dev/null 2>&1; then
    git merge --abort >/dev/null 2>&1 || true
    echo "MODE=SKIP"
    echo "REASON=merge-conflict"
    return 0
  fi
  [ -f "$CONF" ] || { err "no $CONF after merging main"; exit 3; }
  pin="$(pin_of <"$CONF")"
  [ -n "$pin" ] || { err "no schemas pin after merging main"; exit 3; }
  c=0; contains "$a" "$pin" || c=$?
  case "$c" in
    0)
      if [ "$(git rev-parse HEAD)" = "$head" ]; then
        echo "MODE=SKIP"; echo "REASON=contained"
      else
        echo "MODE=MERGE_ONLY"; echo "REASON=main's pin already contains $a"
      fi
      return 0 ;;
    1) ;;
    *) err "containment unknown"; exit 3 ;;
  esac
  rewrite_pin "$CONF" "$a"
  [ "$(pin_of <"$CONF")" = "$a" ] || { err "pin rewrite did not take"; exit 3; }
  echo "MODE=FOLLOW"
  echo "REASON=pin $pin -> $a"
}

# --- compute: lock-and-commit ----------------------------------------------
cmd_lock_and_commit() {
  local dir="$1" meta="$2" out="$3" mode="$4" head a n m new changed
  head="$(meta_get "$meta" head_sha)"; a="$(meta_get "$meta" a)"
  n="$(meta_get "$meta" partner)"; m="$(meta_get "$meta" pr)"
  mkdir -p "$out"
  out="$(cd "$out" && pwd)"
  cd "$dir"
  case "$mode" in
    FOLLOW)
      bash -c "${SPF_LOCK_CMD:-cargo metadata --format-version 1 > /dev/null && cargo metadata --locked --format-version 1 > /dev/null}" ||
        { err "lock refresh failed"; exit 3; }
      changed="$(git status --porcelain --untracked-files=no | awk '{print $2}' | LC_ALL=C sort | tr '\n' ' ')"
      case "$changed" in
        # LC_ALL=C order: `.github/…` sorts before `Cargo.lock`.
        "$CONF " | "$CONF Cargo.lock ") ;;
        *) err "unexpected working-tree changes: '$changed'"; exit 3 ;;
      esac
      git add -- "$CONF"
      [ -n "$(git status --porcelain -- Cargo.lock)" ] && git add -- Cargo.lock
      git -c user.name="$BOT_NAME" -c user.email="$BOT_EMAIL" commit -q \
        -m "chore: follow qontinui-schemas#$n — pin qontinui-schemas to its land commit ${a:0:9}" \
        -m "qontinui/qontinui-schemas#$n landed as $a. This PR adapts to it, so its merge-candidate build must compile that commit rather than the pin it branched with. Moved by .github/workflows/schemas-pair-follow.yml (plan 2026-08-31-schemas-releases-strand-consumer-cargo-locks §7.7)." \
        -m "$TRAILER_KEY: $SCHEMAS#$n" ;;
    MERGE_ONLY) ;;
    *) err "lock-and-commit: mode '$mode' has nothing to commit"; exit 3 ;;
  esac
  new="$(git rev-parse HEAD)"
  [ "$new" != "$head" ] || { err "nothing was committed"; exit 3; }
  git bundle create -q "$out/pair-follow.bundle" "$head..HEAD" 2>/dev/null ||
    git bundle create "$out/pair-follow.bundle" "$head..HEAD" >/dev/null
  jq --arg new "$new" --arg mode "$mode" '. + {new_sha:$new, mode:$mode}' "$meta" >"$out/meta.json"
  echo "NEW_SHA=$new"
}

# --- push -----------------------------------------------------------------
cmd_push() {
  local out="$1" scratch="$2" meta m n head ref main new mode url fetch_url
  out="$(cd "$out" && pwd)"
  meta="$out/meta.json"
  m="$(meta_get "$meta" pr)"; n="$(meta_get "$meta" partner)"; head="$(meta_get "$meta" head_sha)"
  ref="$(meta_get "$meta" head_ref)"; main="$(meta_get "$meta" main_sha)"
  new="$(meta_get "$meta" new_sha)"; mode="$(meta_get "$meta" mode)"
  [[ "$m" =~ ^[0-9]+$ && "$n" =~ ^[0-9]+$ ]] && is_sha "$head" && is_sha "$main" && is_sha "$new" && [ -n "$ref" ] ||
    { err "push: bad meta"; exit 3; }
  if [ "${SPF_KILL_SWITCH:-}" = "off" ]; then echo "RESULT=SKIP kill-switch"; return 0; fi

  fetch_url="${SPF_FETCH_URL:-https://github.com/$RUNNER.git}"
  url="${SPF_PUSH_URL:-https://github.com/$RUNNER.git}"
  rm -rf "$scratch"
  if [ -n "${SPF_FETCH_FILTER-blob:none}" ]; then
    git clone -q --bare --filter="${SPF_FETCH_FILTER-blob:none}" "$fetch_url" "$scratch"
  else
    git clone -q --bare "$fetch_url" "$scratch"
  fi
  cd "$scratch"
  git fetch -q origin "$head" "$main" 2>/dev/null || git fetch -q "$fetch_url" "$head" "$main"
  git bundle verify -q "$out/pair-follow.bundle" >/dev/null 2>&1 || { err "bundle does not verify against $head"; exit 3; }
  git fetch -q "$out/pair-follow.bundle" "HEAD:refs/pf/new" || { err "bundle fetch failed"; exit 3; }
  [ "$(git rev-parse refs/pf/new)" = "$new" ] || { err "bundle tip is not the recorded new_sha"; exit 3; }

  # Structure: at most one merge commit (parents head, main; tree = git's own
  # merge of the two), then, for FOLLOW, exactly one pin commit touching only the
  # conf (and Cargo.lock) and carrying the trailer. Anything else is refused.
  local prev="$head" merges=0 pins=0 c parents np names tree want
  # --first-parent: the merge's second parent pulls main's own commits into
  # head..new, and those are verified by the merge-tree check, not listed as ours.
  for c in $(git rev-list --reverse --first-parent "$head..$new"); do
    parents="$(git rev-list --parents -n 1 "$c" | cut -d' ' -f2-)"
    np="$(wc -w <<<"$parents")"
    if [ "$np" -eq 2 ]; then
      [ "$merges" -eq 0 ] && [ "$pins" -eq 0 ] && [ "$parents" = "$prev $main" ] ||
        { err "patch-scope: unexpected merge commit $c"; exit 3; }
      want="$(git merge-tree --write-tree "$prev" "$main" 2>/dev/null | head -1)" ||
        { err "patch-scope: main does not merge cleanly into $prev"; exit 3; }
      tree="$(git rev-parse "$c^{tree}")"
      [ "$tree" = "$want" ] || { err "patch-scope: merge commit $c is not git's merge of $prev and main"; exit 3; }
      merges=$((merges + 1))
    elif [ "$np" -eq 1 ] && [ "$parents" = "$prev" ]; then
      names="$(git diff --name-only "$prev" "$c" | LC_ALL=C sort | tr '\n' ' ')"
      case "$names" in
        "$CONF " | "$CONF Cargo.lock ") ;;
        *) err "patch-scope: pin commit $c touches '$names'"; exit 3 ;;
      esac
      git log -1 --format=%B "$c" | grep -qx "$TRAILER_KEY: $SCHEMAS#$n" ||
        { err "patch-scope: pin commit $c lacks the $TRAILER_KEY trailer"; exit 3; }
      pins=$((pins + 1))
      [ "$pins" -eq 1 ] || { err "patch-scope: more than one pin commit"; exit 3; }
    else
      err "patch-scope: commit $c has unexpected parents"
      exit 3
    fi
    prev="$c"
  done
  case "$mode" in
    FOLLOW) [ "$pins" -eq 1 ] || { err "patch-scope: FOLLOW without a pin commit"; exit 3; } ;;
    MERGE_ONLY) [ "$pins" -eq 0 ] && [ "$merges" -eq 1 ] || { err "patch-scope: MERGE_ONLY must be one merge commit"; exit 3; } ;;
    *) err "patch-scope: mode '$mode'"; exit 3 ;;
  esac

  # Re-read the head immediately before touching anything.
  local mj cur
  mj="$(gh_get "repos/$RUNNER/pulls/$m")" || { err "re-read $RUNNER#$m failed"; exit 3; }
  cur="$(jq -r '.head.sha' <<<"$mj")"
  if [ "$cur" != "$head" ]; then echo "RESULT=ABORT head-moved ($head -> $cur)"; return 0; fi

  # A still-present form-2 label would red the pushed head on checkout-sibling's
  # not-open refusal. Removed with GITHUB_TOKEN, which triggers no rerun.
  local lbl enc
  while read -r lbl; do
    [ -n "$lbl" ] || continue
    enc="$(jq -rn --arg k "$lbl" '$k|@uri')"
    gh_write DELETE "repos/$RUNNER/issues/$m/labels/$enc" || { err "could not remove label '$lbl'"; exit 3; }
  done < <(label_names "$mj" | grep -E "^coord:downstream-of=(qontinui/)?qontinui-schemas#$n\$" || true)

  local pushcfg=()
  case "$url" in
    https://*)
      [ -n "${SPF_PUSH_TOKEN:-}" ] || { err "no push token: CLORINDE_AUTOCOMMIT_TOKEN is required (a GITHUB_TOKEN push fires no pull_request CI)"; exit 3; }
      local auth
      auth="$(printf 'x-access-token:%s' "$SPF_PUSH_TOKEN" | base64 -w0)"
      [ -n "${GITHUB_ACTIONS:-}" ] && echo "::add-mask::$auth"
      pushcfg=(-c "http.https://github.com/.extraheader=AUTHORIZATION: basic $auth") ;;
  esac
  local perr
  perr="$(mktemp)"
  if ! git "${pushcfg[@]}" push --force-with-lease="refs/heads/$ref:$head" "$url" "$new:refs/heads/$ref" 2>"$perr"; then
    if grep -qiE 'stale info|fetch first|rejected' "$perr"; then
      rm -f "$perr"
      echo "RESULT=ABORT lease-rejected"
      return 0
    fi
    err "push failed: $(tr '\n' ' ' <"$perr")"
    rm -f "$perr"
    exit 3
  fi
  rm -f "$perr"

  gh_write POST "repos/$RUNNER/issues/$m/comments" -f "body=Schemas pair-follow: \`qontinui/qontinui-schemas#$n\` landed, and this PR adapts to it. Its merge-candidate build compiles the commit pinned in \`.github/sibling-pins.conf\`, not this PR's declaration, so the pin (and \`Cargo.lock\`) were moved to the land commit in \`${new:0:9}\` (mode $mode, on top of \`${head:0:9}\`). Plan 2026-08-31-schemas-releases-strand-consumer-cargo-locks §7.7; kill switch: repository variable SCHEMAS_PAIR_FOLLOW=off." ||
    err "comment failed (the push itself succeeded)"
  echo "RESULT=PUSHED $new"
}

case "${1:-}" in
  scan) cmd_scan ;;
  decide) [ $# -eq 3 ] || { err "usage: decide <M> <N>"; exit 2; }; cmd_decide "$2" "$3" ;;
  merge-and-pin) [ $# -eq 3 ] || { err "usage: merge-and-pin <runner-dir> <meta.json>"; exit 2; }; cmd_merge_and_pin "$2" "$3" ;;
  lock-and-commit) [ $# -eq 5 ] || { err "usage: lock-and-commit <runner-dir> <meta.json> <out-dir> <MODE>"; exit 2; }; cmd_lock_and_commit "$2" "$3" "$4" "$5" ;;
  push) [ $# -eq 3 ] || { err "usage: push <out-dir> <scratch-dir>"; exit 2; }; cmd_push "$2" "$3" ;;
  *) err "usage: schemas-pair-follow.sh {scan | decide M N | merge-and-pin DIR META | lock-and-commit DIR META OUT MODE | push OUT SCRATCH}"; exit 2 ;;
esac
