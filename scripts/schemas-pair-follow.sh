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
# adaptation pair is a runner PR M adapting to a breaking schemas PR N, declared
# as `coord:downstream-of=[qontinui/]qontinui-schemas#N` on M (form 2) or
# `coord:upstream-of=[qontinui/]qontinui-runner#M` on N (form 1). Once N lands:
#   - coord strips M's label;
#   - checkout-sibling refuses a declaration that is no longer open;
#   - a merge-candidate push carries no PR number, so it reads no declaration at all.
# M's rerun and its candidate build therefore compile the OLD pin, and M stays
# red until someone moves the pin and Cargo.lock inside M. This script makes that
# one move, and refuses whenever it cannot prove the move is safe.
#
# ---------------------------------------------------------------------------
# TRUST
# ---------------------------------------------------------------------------
# `decide` and `scan` run main's code over API reads, and their output is the
# TRUSTED plan item. `merge-and-pin` and `lock-and-commit` run in an unprivileged
# job. They execute no code from the PR's tree: git merges data, cargo runs from
# outside the checkout with main's toolchain (so neither the tree's
# rust-toolchain.toml nor its .cargo/config.toml is honoured), and
# checkout-sibling comes from main. `push` treats that job's artifact as
# UNTRUSTED. It takes every target (PR, ref, head, main, land commit) from the
# trusted plan item and only a candidate commit id from the artifact. It then
# re-derives and re-verifies everything before pushing.
#
# ---------------------------------------------------------------------------
# SUBCOMMANDS
# ---------------------------------------------------------------------------
#   scan
#     Print one "M N" candidate per line, from both forms.
#   decide <M> <N>
#     Print ONE JSON object: FOLLOW / SKIP / UNKNOWN. Always exits 0; the caller
#     counts UNKNOWNs.
#   merge-and-pin <runner-dir> <plan-item.json>
#     Merge runner main into M's head, then move the pin to A unless the merge
#     already contains it. Print MODE= and REASON=.
#   lock-and-commit <runner-dir> <plan-item.json> <out-dir> <MODE>
#     Refresh Cargo.lock, assert scope, commit, and write
#     <out-dir>/pair-follow.bundle and <out-dir>/meta.json.
#   push <plan-item.json> <out-dir> <scratch-dir>
#     Verify the bundle against the TRUSTED plan item, re-read the PR and its
#     claims, and push with --force-with-lease. Print RESULT=.
#
# Exit codes: 0 = a named outcome (FOLLOW / MERGE_ONLY / SKIP / ABORT / PUSHED / REFUSED);
#             3 = UNKNOWN or refused (the lane goes red); 2 = usage.
#
# ENV
#   SPF_FIXTURES             fixture root (gh/<name>, coord/<name>, writes.log); no network
#   SPF_KILL_SWITCH          repository variable SCHEMAS_PAIR_FOLLOW; 'off' disables
#   COORD_CLAIMS_READ_TOKEN  optional bearer for coord claim reads (plan §7.7)
#   SPF_COORD_URL            default https://coord.qontinui.io
#   SPF_RUNNER_REPO          default qontinui/qontinui-runner
#   SPF_TOOLCHAIN            rust toolchain for the lock refresh (main's channel)
#   SPF_LOCK_CMD             test override for the lock refresh (runs in the runner dir)
#   SPF_PUSH_TOKEN           PAT for the push (CLORINDE_AUTOCOMMIT_TOKEN)
#   SPF_PUSH_URL / SPF_FETCH_URL  remotes (default https://github.com/<runner>.git)
#   SPF_FETCH_FILTER         verify-clone filter (default blob:none; '' = none)
set -euo pipefail

RUNNER="${SPF_RUNNER_REPO:-qontinui/qontinui-runner}"
RUNNER_NAME="${RUNNER#*/}"
SCHEMAS="qontinui/qontinui-schemas"
CONF=".github/sibling-pins.conf"
COORD="${SPF_COORD_URL:-https://coord.qontinui.io}"
# The coord GitHub App: it posts the land announcement (render_ff_land_comment)
# and strips waiting-side dep labels (DepLabelStripHook).
COORD_BOT="qontinui-merge-orchestrator[bot]"
TRAILER_KEY="Schemas-Pair-Follow"
# Posted once when a follow needs a human (a lock change pair-follow will not make).
refused_marker() { printf '<!-- schemas-pair-follow:refused a=%s -->' "$1"; }
BOT_NAME="github-actions[bot]"
BOT_EMAIL="41898283+github-actions[bot]@users.noreply.github.com"

err() { printf 'schemas-pair-follow: %s\n' "$*" >&2; }
fxname() { printf '%s' "$1" | sed 's#[^A-Za-z0-9._-]#_#g'; }
is_sha() { [[ "$1" =~ ^[0-9a-f]{40}$ ]]; }
is_num() { [[ "$1" =~ ^[0-9]+$ ]]; }
form2_re() { printf '^coord:downstream-of=(qontinui/)?qontinui-schemas#%s$' "$1"; }

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
    if [ -f "$SPF_FIXTURES/write-fail" ]; then return 1; fi
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
  # No `-e`: a free claim IS `"holder": null`, and `jq -e` exits 1 on a null
  # result. A wrong shape still fails, through error().
  if ! holder="$(printf '%s' "$body" | jq -c 'if type == "object" and has("holder") then .holder else error("shape") end' 2>/dev/null)"; then
    err "claim read $kind/$key: unparseable body. UNKNOWN, not 'no claim'."
    return 3
  fi
  if [ "$holder" = "null" ]; then echo free; else echo held; fi
}

# first_held_claim M REF -> prints the held kind, or nothing. rc 3 = UNKNOWN.
first_held_claim() {
  local m="$1" ref="$2" kind key st kind_key
  for kind_key in "branch_name|$ref" "ci_wait|$RUNNER_NAME#$m" "ci_wait|$RUNNER#$m" "repo_branch|$RUNNER_NAME:$ref"; do
    kind="${kind_key%%|*}"
    key="${kind_key#*|}"
    st="$(claim_state "$kind" "$key")" || return 3
    if [ "$st" = "held" ]; then echo "$kind"; return 0; fi
  done
  return 0
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
# compare/<base>...<head> reports HEAD relative to BASE, so a PIN (head) that
# contains A (base) reads `ahead` or `identical`.
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

# land_commit N_JSON -> full sha of N's land commit A, verified on schemas main. rc 3 UNKNOWN.
land_commit() {
  local nj="$1" n sha="" short closed st
  n="$(jq -r .number <<<"$nj")"
  if [ "$(jq -r '.merged_at // empty' <<<"$nj")" != "" ]; then
    sha="$(jq -r '.merge_commit_sha // empty' <<<"$nj")"
  else
    local comments
    comments="$(gh_get "repos/$SCHEMAS/issues/$n/comments?per_page=100")" || return 3
    short="$(jq -r --arg bot "$COORD_BOT" '
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

# stripped_by_coord EVENTS_JSON RE -> rc 0 when the coord App removed a label matching RE.
# A label an author withdrew by hand is NOT a declaration (plan §7.7).
stripped_by_coord() {
  jq -r --arg bot "$COORD_BOT" '.[] | select(.event == "unlabeled" and (.actor.login // "") == $bot) | .label.name' <<<"$1" | grep -Eq "$2"
}

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

  is_num "$m" && is_num "$n" || { err "decide: M and N must be numbers"; exit 2; }
  if [ "${SPF_KILL_SWITCH:-}" = "off" ]; then skip kill-switch; return; fi

  local nj mj
  nj="$(gh_get "repos/$SCHEMAS/pulls/$n")" || { unknown "read $SCHEMAS#$n failed"; return; }
  mj="$(gh_get "repos/$RUNNER/pulls/$m")" || { unknown "read $RUNNER#$m failed"; return; }

  # Declaration: which form, if any, pairs M with N with schemas LEADING.
  local re_f1="^coord:upstream-of=(qontinui/)?$RUNNER_NAME#$m\$"
  local re_f2; re_f2="$(form2_re "$n")"
  local re_t_m="^coord:upstream-of=(qontinui/)?qontinui-schemas#$n\$"
  local re_t_n="^coord:downstream-of=(qontinui/)?$RUNNER_NAME#$m\$"
  local leads=false
  has_label_re "$nj" "$re_f1" && leads=true
  if has_label_re "$mj" "$re_f2"; then leads=true; label_present=true; fi
  if [ "$leads" != true ]; then
    local events
    events="$(gh_get "repos/$RUNNER/issues/$m/events?per_page=100")" || { unknown "read $RUNNER#$m events failed"; return; }
    stripped_by_coord "$events" "$re_f2" && leads=true
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
  # A stacked PR would get main merged into its branch: its base is not ours to widen.
  [ "$(jq -r '.base.ref // ""' <<<"$mj")" = "main" ] || { skip pr-not-on-main; return; }

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

  local held
  if ! held="$(first_held_claim "$m" "$head_ref")"; then unknown "claim read unknown"; return; fi
  if [ -n "$held" ]; then skip "live-claim:$held"; return; fi

  # Already refused for this same land commit and handed to a human: do not
  # recompute and re-refuse every cycle (that would red the lane forever).
  local pr_comments
  pr_comments="$(gh_get "repos/$RUNNER/issues/$m/comments?per_page=100")" || { unknown "read $RUNNER#$m comments failed"; return; }
  if jq -r --arg bot "$BOT_NAME" '.[] | select(.user.login == $bot) | .body' <<<"$pr_comments" | grep -qF "$(refused_marker "$a")"; then
    skip refused-awaiting-human
    return
  fi

  action=FOLLOW
  reason="$SCHEMAS#$n landed as ${a:0:9}"
  emit
}

# --- scan ------------------------------------------------------------------
cmd_scan() {
  if [ "${SPF_KILL_SWITCH:-}" = "off" ]; then err "kill switch SCHEMAS_PAIR_FOLLOW=off"; return 0; fi
  local page=1 open="[]" chunk
  while :; do
    chunk="$(gh_get "repos/$RUNNER/pulls?state=open&per_page=100&page=$page")" || { err "list open runner PRs failed"; exit 3; }
    open="$(jq -c --argjson c "$chunk" '. + $c' <<<"$open")"
    if [ "$(jq length <<<"$chunk")" -lt 100 ]; then break; fi
    page=$((page + 1))
  done
  [ "$(jq length <<<"$open")" -gt 0 ] || return 0
  local oldest numbers
  oldest="$(jq -r '[.[].created_at] | min' <<<"$open")"
  numbers="$(jq -r '.[].number' <<<"$open")"

  {
    # Form 2, still labelled (a strip the reconciler has not reached yet).
    jq -r '.[] | .number as $m | .labels[]?.name
      | capture("^coord:downstream-of=(qontinui/)?qontinui-schemas#(?<n>[0-9]+)$")? | "\($m) \(.n)"' <<<"$open"

    # Form 2, already stripped by coord, recovering a missed detector dispatch
    # (kill switch off at strip time, a 403, the workflow not yet on main).
    local m events
    for m in $numbers; do
      events="$(gh_get "repos/$RUNNER/issues/$m/events?per_page=100")" || { err "read $RUNNER#$m events failed"; exit 3; }
      jq -r --arg bot "$COORD_BOT" --arg m "$m" '
        .[] | select(.event == "unlabeled" and (.actor.login // "") == $bot) | .label.name
        | capture("^coord:downstream-of=(qontinui/)?qontinui-schemas#(?<n>[0-9]+)$")? | "\($m) \(.n)"' <<<"$events"
    done

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

# rewrite_pin CONF NEW -> replace the SHA token on the schemas line only.
rewrite_pin() {
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
  is_sha "$head" && is_sha "$main" && is_sha "$a" || { err "merge-and-pin: bad plan item"; exit 3; }
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
  local dir="$1" meta="$2" out="$3" mode="$4" head a n new changed
  head="$(meta_get "$meta" head_sha)"; a="$(meta_get "$meta" a)"; n="$(meta_get "$meta" partner)"
  mkdir -p "$out"
  out="$(cd "$out" && pwd)"
  dir="$(cd "$dir" && pwd)"
  cd "$dir"
  case "$mode" in
    FOLLOW)
      if [ -n "${SPF_LOCK_CMD:-}" ]; then
        bash -c "$SPF_LOCK_CMD" || { err "lock refresh failed"; exit 3; }
      else
        [ -n "${SPF_TOOLCHAIN:-}" ] || { err "SPF_TOOLCHAIN (main's rust channel) is required for the lock refresh"; exit 3; }
        # From OUTSIDE the checkout, with an explicit toolchain: rustup's override
        # and cargo's config discovery both key on the working directory, so the
        # PR's rust-toolchain.toml and .cargo/config.toml are not honoured.
        # `cargo metadata` runs no build scripts. Resolve-only, then --locked.
        (cd "${RUNNER_TEMP:-${TMPDIR:-/tmp}}" &&
          cargo "+$SPF_TOOLCHAIN" metadata --manifest-path "$dir/Cargo.toml" --format-version 1 >/dev/null &&
          cargo "+$SPF_TOOLCHAIN" metadata --locked --manifest-path "$dir/Cargo.toml" --format-version 1 >/dev/null) ||
          { err "lock refresh failed"; exit 3; }
      fi
      changed="$(git status --porcelain --untracked-files=no | awk '{print $2}' | LC_ALL=C sort | tr '\n' ' ')"
      case "$changed" in
        # LC_ALL=C order: `.github/…` sorts before `Cargo.lock`.
        "$CONF " | "$CONF Cargo.lock ") ;;
        *) err "unexpected working-tree changes: '$changed'"; exit 3 ;;
      esac
      git add -- "$CONF"
      if [ -n "$(git status --porcelain -- Cargo.lock)" ]; then git add -- Cargo.lock; fi
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
  jq -n --arg new "$new" '{new_sha:$new}' >"$out/meta.json"
  echo "NEW_SHA=$new"
}

# --- push: verification helpers -------------------------------------------
# lock_ok PREV C -> rc 0 when Cargo.lock at C differs from PREV only in path
# (source-less) packages' versions and dependency lists. Every registry or git
# package entry must be byte-for-byte identical, as must the set of path packages
# and the top-level keys. A partner that adds a registry dependency is therefore
# refused, and a human finishes that follow.
lock_ok() {
  local prev="$1" c="$2" t rc=0
  t="$(mktemp -d)"
  git show "$prev:Cargo.lock" >"$t/a" 2>/dev/null || : >"$t/a"
  git show "$c:Cargo.lock" >"$t/b" 2>/dev/null || : >"$t/b"
  python3 - "$t/a" "$t/b" <<'PY' || rc=$?
import json, sys, tomllib
def load(p):
    with open(p, 'rb') as f:
        return tomllib.load(f)
def split(d):
    pk = d.get('package', [])
    reg = sorted(json.dumps(p, sort_keys=True) for p in pk if 'source' in p)
    path = sorted(p.get('name', '') for p in pk if 'source' not in p)
    top = {k: v for k, v in d.items() if k != 'package'}
    return top, reg, path
try:
    a, b = load(sys.argv[1]), load(sys.argv[2])
except Exception as e:
    print('Cargo.lock does not parse: %s' % e); sys.exit(1)
ta, ra, pa = split(a); tb, rb, pb = split(b)
if ta != tb: print('Cargo.lock top-level keys changed'); sys.exit(1)
if ra != rb: print('a registry/git package entry in Cargo.lock changed'); sys.exit(1)
if pa != pb: print('the set of path packages in Cargo.lock changed'); sys.exit(1)
PY
  rm -rf "$t"
  return "$rc"
}

# --- push -----------------------------------------------------------------
cmd_push() {
  local trusted="$1" out="$2" scratch="$3"
  local m n a head ref main new url fetch_url
  # TRUSTED: the plan item (main's code over API reads). UNTRUSTED: the artifact,
  # from which only a candidate commit id is taken.
  m="$(meta_get "$trusted" pr)"; n="$(meta_get "$trusted" partner)"; a="$(meta_get "$trusted" a)"
  head="$(meta_get "$trusted" head_sha)"; ref="$(meta_get "$trusted" head_ref)"; main="$(meta_get "$trusted" main_sha)"
  is_num "$m" && is_num "$n" && is_sha "$a" && is_sha "$head" && is_sha "$main" || { err "push: bad plan item"; exit 3; }
  if [ -z "$ref" ] || [[ "$ref" == -* || "$ref" == *:* ]] || ! git check-ref-format "refs/heads/$ref"; then
    err "push: head ref '$ref' is not a safe branch name"
    exit 3
  fi
  out="$(cd "$out" && pwd)"
  new="$(jq -r '.new_sha // empty' "$out/meta.json" 2>/dev/null)"
  is_sha "$new" || { err "push: artifact carries no commit id"; exit 3; }
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
  git fetch -q origin "$head" "$main" 2>/dev/null || { err "cannot fetch the trusted head and main"; exit 3; }
  git bundle verify -q "$out/pair-follow.bundle" >/dev/null 2>&1 || { err "patch-scope: bundle does not verify against the trusted head"; exit 3; }
  git fetch -q "$out/pair-follow.bundle" "HEAD:refs/pf/new" || { err "patch-scope: bundle fetch failed"; exit 3; }
  [ "$(git rev-parse refs/pf/new)" = "$new" ] || { err "patch-scope: bundle tip is not the artifact's commit id"; exit 3; }
  git merge-base --is-ancestor "$head" "$new" || { err "patch-scope: $new does not descend from the trusted head $head"; exit 3; }

  # Structure: at most one merge commit (parents trusted head and trusted main;
  # tree = git's own merge of the two), then at most one pin commit that sets the
  # pin to exactly A, keeps Cargo.lock's registry entries identical, and carries
  # the trailer. --first-parent: the merge's second parent pulls main's own
  # commits into head..new, and those are covered by the merge-tree check.
  local prev="$head" merges=0 pins=0 c parents np names want tree t
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
      [ "$pins" -eq 0 ] || { err "patch-scope: more than one pin commit"; exit 3; }
      names="$(git diff --name-only "$prev" "$c" | LC_ALL=C sort | tr '\n' ' ')"
      case "$names" in
        "$CONF " | "$CONF Cargo.lock ") ;;
        *) err "patch-scope: pin commit $c touches '$names'"; exit 3 ;;
      esac
      t="$(mktemp -d)"
      git show "$prev:$CONF" >"$t/want"
      rewrite_pin "$t/want" "$a"
      git show "$c:$CONF" >"$t/got"
      cmp -s "$t/want" "$t/got" || { rm -rf "$t"; err "patch-scope: pin commit $c does not set the pin to exactly $a"; exit 3; }
      rm -rf "$t"
      local why
      if ! why="$(lock_ok "$prev" "$c")"; then
        refuse_for_human "$m" "$n" "$a" "$why"
        return 0
      fi
      git log -1 --format=%B "$c" | grep -qx "$TRAILER_KEY: $SCHEMAS#$n" ||
        { err "patch-scope: pin commit $c lacks the $TRAILER_KEY trailer"; exit 3; }
      pins=$((pins + 1))
    else
      err "patch-scope: commit $c has unexpected parents"
      exit 3
    fi
    prev="$c"
  done
  [ "$prev" = "$new" ] || { err "patch-scope: first-parent walk did not reach $new"; exit 3; }
  [ $((merges + pins)) -ge 1 ] || { err "patch-scope: nothing to push"; exit 3; }
  local mode=MERGE_ONLY
  [ "$pins" -eq 1 ] && mode=FOLLOW
  # The mode comes from content, never from the artifact: the pushed tip's pin must contain A.
  local tip_pin cc=0
  tip_pin="$(git show "$new:$CONF" | pin_of)"
  [ -n "$tip_pin" ] || { err "patch-scope: pushed tip has no schemas pin"; exit 3; }
  contains "$a" "$tip_pin" || cc=$?
  [ "$cc" -eq 0 ] || { err "patch-scope: the pushed tip's pin $tip_pin does not contain $a"; exit 3; }

  # Re-read the PR immediately before touching anything.
  local mj
  mj="$(gh_get "repos/$RUNNER/pulls/$m")" || { err "re-read $RUNNER#$m failed"; exit 3; }
  if [ "$(jq -r .state <<<"$mj")" != "open" ] ||
    [ "$(jq -r '.head.repo.full_name // ""' <<<"$mj")" != "$RUNNER" ] ||
    [ "$(jq -r '.head.ref // ""' <<<"$mj")" != "$ref" ] ||
    [ "$(jq -r '.base.ref // ""' <<<"$mj")" != "main" ]; then
    echo "RESULT=ABORT pr-changed"
    return 0
  fi
  local cur
  cur="$(jq -r '.head.sha' <<<"$mj")"
  if [ "$cur" != "$head" ]; then echo "RESULT=ABORT head-moved ($head -> $cur)"; return 0; fi
  local held
  held="$(first_held_claim "$m" "$ref")" || { err "claim re-read unknown"; exit 3; }
  if [ -n "$held" ]; then echo "RESULT=ABORT live-claim:$held"; return 0; fi

  # A still-present form-2 label would red the pushed head on checkout-sibling's
  # not-open refusal. It is removed with GITHUB_TOKEN (which triggers no rerun)
  # and put back if the push is then refused.
  local removed=() lbl enc
  while read -r lbl; do
    [ -n "$lbl" ] || continue
    enc="$(jq -rn --arg k "$lbl" '$k|@uri')"
    gh_write DELETE "repos/$RUNNER/issues/$m/labels/$enc" || { err "could not remove label '$lbl'"; exit 3; }
    removed+=("$lbl")
  done < <(label_names "$mj" | grep -E "$(form2_re "$n")" || true)

  local pushcfg=()
  case "$url" in
    https://*)
      [ -n "${SPF_PUSH_TOKEN:-}" ] || { err "no push token: CLORINDE_AUTOCOMMIT_TOKEN is required (a GITHUB_TOKEN push fires no pull_request CI)"; restore_labels "$m" "${removed[@]+"${removed[@]}"}"; exit 3; }
      local auth
      auth="$(printf 'x-access-token:%s' "$SPF_PUSH_TOKEN" | base64 -w0)"
      if [ -n "${GITHUB_ACTIONS:-}" ]; then echo "::add-mask::$auth"; fi
      pushcfg=(-c "http.https://github.com/.extraheader=AUTHORIZATION: basic $auth") ;;
  esac
  local perr
  perr="$(mktemp)"
  if ! git "${pushcfg[@]+"${pushcfg[@]}"}" push --force-with-lease="refs/heads/$ref:$head" "$url" "$new:refs/heads/$ref" 2>"$perr"; then
    if grep -qiE 'stale info|fetch first|rejected' "$perr"; then
      rm -f "$perr"
      restore_labels "$m" "${removed[@]+"${removed[@]}"}" || exit 3
      echo "RESULT=ABORT lease-rejected"
      return 0
    fi
    err "push failed: $(sed 's/basic [A-Za-z0-9+/=]*/basic ***/g' "$perr" | tr '\n' ' ')"
    rm -f "$perr"
    restore_labels "$m" "${removed[@]+"${removed[@]}"}" || true
    exit 3
  fi
  rm -f "$perr"

  gh_write POST "repos/$RUNNER/issues/$m/comments" -f "body=Schemas pair-follow: \`qontinui/qontinui-schemas#$n\` landed, and this PR adapts to it. Its merge-candidate build compiles the commit pinned in \`.github/sibling-pins.conf\`, not this PR's declaration, so the pin (and \`Cargo.lock\`) were moved to the land commit \`${a:0:9}\` in \`${new:0:9}\` (mode $mode, on top of \`${head:0:9}\`). Plan 2026-08-31-schemas-releases-strand-consumer-cargo-locks §7.7; kill switch: repository variable SCHEMAS_PAIR_FOLLOW=off." ||
    err "comment failed (the push itself succeeded)"
  echo "RESULT=PUSHED $new"
}

# refuse_for_human M N A WHY -> the follow needs a Cargo.lock change pair-follow
# will not make (a registry entry moved: the partner changed a schemas crate's
# dependencies). Tell the PR ONCE with a marker `decide` honours, and end with
# a named outcome rather than red, so one such PR cannot red the lane every cycle.
refuse_for_human() {
  local m="$1" n="$2" a="$3" why="$4" marker comments
  marker="$(refused_marker "$a")"
  comments="$(gh_get "repos/$RUNNER/issues/$m/comments?per_page=100")" || { err "read $RUNNER#$m comments failed"; exit 3; }
  if ! jq -r '.[].body' <<<"$comments" | grep -qF "$marker"; then
    gh_write POST "repos/$RUNNER/issues/$m/comments" -f "body=Schemas pair-follow could not finish this PR by itself. \`qontinui/qontinui-schemas#$n\` landed as \`${a:0:9}\`, and moving this PR's pin there needs a \`Cargo.lock\` change pair-follow is not allowed to make: $why. Please move the pin in \`.github/sibling-pins.conf\` to \`$a\` and refresh \`Cargo.lock\` in the same commit (\`cargo metadata --format-version 1\` with that schemas commit checked out beside the runner). $marker" ||
      { err "could not post the refusal comment"; exit 3; }
  fi
  # To stderr: stdout carries only the RESULT= line the caller reads (the runner
  # still parses workflow commands on either stream).
  if [ -n "${GITHUB_ACTIONS:-}" ]; then echo "::warning::#$m needs a human: $why" >&2; fi
  echo "RESULT=REFUSED lock-registry-change"
}

# restore_labels M LABEL... -> put back labels removed before a push that did not land.
restore_labels() {
  local m="$1" lbl
  shift
  for lbl in "$@"; do
    gh_write POST "repos/$RUNNER/issues/$m/labels" -f "labels[]=$lbl" || { err "could not restore label '$lbl' on #$m — restore it by hand"; return 1; }
  done
  return 0
}

case "${1:-}" in
  scan) cmd_scan ;;
  decide) [ $# -eq 3 ] || { err "usage: decide <M> <N>"; exit 2; }; cmd_decide "$2" "$3" ;;
  merge-and-pin) [ $# -eq 3 ] || { err "usage: merge-and-pin <runner-dir> <plan-item.json>"; exit 2; }; cmd_merge_and_pin "$2" "$3" ;;
  lock-and-commit) [ $# -eq 5 ] || { err "usage: lock-and-commit <runner-dir> <plan-item.json> <out-dir> <MODE>"; exit 2; }; cmd_lock_and_commit "$2" "$3" "$4" "$5" ;;
  push) [ $# -eq 4 ] || { err "usage: push <plan-item.json> <out-dir> <scratch-dir>"; exit 2; }; cmd_push "$2" "$3" "$4" ;;
  *) err "usage: schemas-pair-follow.sh {scan | decide M N | merge-and-pin DIR ITEM | lock-and-commit DIR ITEM OUT MODE | push ITEM OUT SCRATCH}"; exit 2 ;;
esac
