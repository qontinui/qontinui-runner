#!/usr/bin/env bash
# Fixture tests for scripts/schemas-pair-follow.sh — no network.
#
# GitHub and coord responses are files under a per-case fixture root
# (SPF_FIXTURES), and every git operation runs against local repos. The cases are
# the ones plan 2026-08-31-schemas-releases-strand-consumer-cargo-locks §7.7 item 5
# requires, led by the STUCK-PAIR shape that motivates the lane: a form-2 label
# already stripped, a partner closed-unmerged with `coord:landed`, an old pin, and
# no claim.
#
# Run: bash scripts/tests/schemas-pair-follow/run.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SUT="$(cd "$HERE/../.." && pwd)/schemas-pair-follow.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
fails=0
passes=0

ok() { passes=$((passes + 1)); printf 'ok   - %s\n' "$1"; }
bad() { fails=$((fails + 1)); printf 'FAIL - %s\n' "$1"; [ -n "${2:-}" ] && printf '       %s\n' "$2"; }

R=qontinui/qontinui-runner
SC=qontinui/qontinui-schemas
A=aaaaaaaaa0000000000000000000000000000000   # partner's land commit
OLD=1111111111111111111111111111111111111111  # the pin the PR branched with
NEWER=2222222222222222222222222222222222222222
K=3333333333333333333333333333333333333333    # a later schemas commit
MAIN=4444444444444444444444444444444444444444
HEAD=5555555555555555555555555555555555555555
M2=6666666666666666666666666666666666666666   # a runner-main pin already past A

fxname() { printf '%s' "$1" | sed 's#[^A-Za-z0-9._-]#_#g'; }
gh_fx() { mkdir -p "$FX/gh"; printf '%s' "$2" >"$FX/gh/$(fxname "$1")"; }
coord_fx() { # kind key status body
  mkdir -p "$FX/coord"
  local f; f="$FX/coord/$(fxname "$1:$2")"
  printf '%s' "$3" >"$f.status"
  printf '%s' "$4" >"$f"
}
conf_json() { # pin -> contents-API JSON (base64 content, as GitHub serves it)
  local text
  text="$(printf '# pins\nqontinui/ui-bridge %s\nqontinui/qontinui-schemas %s\n' "$NEWER" "$1")"
  jq -cn --arg c "$(printf '%s\n' "$text" | base64 -w0)" '{content:$c, encoding:"base64"}'
}
new_fx() { FX="$TMP/fx-$1"; rm -rf "$FX"; mkdir -p "$FX/gh" "$FX/coord"; }

# The stuck pair: runner #100 carried coord:downstream-of=qontinui-schemas#50, and
# coord stripped it after #50 fast-forward landed (CLOSED, merged_at null,
# coord:landed, land comment). Both pins are still OLD.
setup_base() {
  new_fx "$1"
  gh_fx "repos/$SC/pulls/50" '{"number":50,"state":"closed","merged_at":null,"merge_commit_sha":null,"closed_at":"2026-09-10T00:00:00Z","base":{"ref":"main"},"labels":[{"name":"coord:landed"}]}'
  gh_fx "repos/$R/pulls/100" "{\"number\":100,\"state\":\"open\",\"head\":{\"sha\":\"$HEAD\",\"ref\":\"agent/x\",\"repo\":{\"full_name\":\"$R\"}},\"base\":{\"ref\":\"main\"},\"labels\":[]}"
  gh_fx "repos/$R/issues/100/events?per_page=100" '[{"event":"labeled","label":{"name":"coord:downstream-of=qontinui-schemas#50"}},{"event":"unlabeled","actor":{"login":"qontinui-merge-orchestrator[bot]"},"label":{"name":"coord:downstream-of=qontinui-schemas#50"}}]'
  gh_fx "repos/$SC/issues/50/comments?per_page=100" '[{"user":{"login":"someone"},"body":"lgtm"},{"user":{"login":"qontinui-merge-orchestrator[bot]"},"body":"✅ **Landed on `main` by coord** as `aaaaaaaaa` (rebase fast-forward)."}]'
  gh_fx "repos/$SC/commits/aaaaaaaaa" "{\"sha\":\"$A\"}"
  gh_fx "repos/$SC/commits/main" "{\"sha\":\"$K\"}"
  gh_fx "repos/$SC/compare/$A...main" '{"status":"ahead"}'
  gh_fx "repos/$R/commits/main" "{\"sha\":\"$MAIN\"}"
  gh_fx "repos/$R/contents/.github/sibling-pins.conf?ref=$MAIN" "$(conf_json "$OLD")"
  gh_fx "repos/$R/contents/.github/sibling-pins.conf?ref=$HEAD" "$(conf_json "$OLD")"
  gh_fx "repos/$SC/compare/$A...$OLD" '{"status":"behind"}'
  gh_fx "repos/$R/issues/100/comments?per_page=100" '[]'
}

LAST=""
expect_decide() { # name action reason-prefix
  local out act rsn
  out="$(SPF_FIXTURES="$FX" SPF_KILL_SWITCH="${KILL:-}" bash "$SUT" decide 100 50 2>"$FX/stderr")"
  act="$(jq -r .action <<<"$out" 2>/dev/null)"
  rsn="$(jq -r .reason <<<"$out" 2>/dev/null)"
  LAST="$out"
  if [ "$act" = "$2" ] && [[ "$rsn" == "$3"* ]]; then
    ok "$1"
  else
    bad "$1" "want $2/$3* got action=$act reason=$rsn; stderr: $(tr '\n' ' ' <"$FX/stderr")"
  fi
}
expect_field() { # name jq-filter expected
  local got
  got="$(jq -r "$2" <<<"$LAST" 2>/dev/null)"
  [ "$got" = "$3" ] && ok "$1" || bad "$1" "$2 = '$got', want '$3'"
}

echo "# decide"

setup_base stuck
expect_decide "stuck pair (form 2 stripped, coord:landed not merged, old pin, no claim) -> FOLLOW" FOLLOW "$SC#50 landed"
expect_field "  ...pins to the land commit A" .a "$A"
expect_field "  ...mode FOLLOW" .mode FOLLOW
expect_field "  ...label no longer present" .label_present false

setup_base laterk
expect_decide "schemas main carries a later commit K -> still FOLLOW" FOLLOW ""
expect_field "  ...pin target is A, never schemas main's head K" .a "$A"

setup_base form1
gh_fx "repos/$R/issues/100/events?per_page=100" '[]'
gh_fx "repos/$SC/pulls/50" '{"number":50,"state":"closed","merged_at":null,"closed_at":"2026-09-10T00:00:00Z","base":{"ref":"main"},"labels":[{"name":"coord:landed"},{"name":"coord:upstream-of=qontinui/qontinui-runner#100"}]}'
expect_decide "form 1 on the schemas PR (owner-qualified) -> FOLLOW" FOLLOW ""

setup_base merged
gh_fx "repos/$R/issues/100/events?per_page=100" '[]'
gh_fx "repos/$R/pulls/100" "{\"number\":100,\"state\":\"open\",\"head\":{\"sha\":\"$HEAD\",\"ref\":\"agent/x\",\"repo\":{\"full_name\":\"$R\"}},\"base\":{\"ref\":\"main\"},\"labels\":[{\"name\":\"coord:downstream-of=qontinui-schemas#50\"}]}"
gh_fx "repos/$SC/pulls/50" "{\"number\":50,\"state\":\"closed\",\"merged_at\":\"2026-09-10T00:00:00Z\",\"merge_commit_sha\":\"$A\",\"closed_at\":\"2026-09-10T00:00:00Z\",\"base\":{\"ref\":\"main\"},\"labels\":[]}"
expect_decide "partner MERGED (not ff-landed), form-2 label still present -> FOLLOW" FOLLOW ""
expect_field "  ...A is merge_commit_sha" .a "$A"
expect_field "  ...label_present true (the push job removes it)" .label_present true

setup_base phantom
gh_fx "repos/$SC/issues/50/comments?per_page=100" '[]'
gh_fx "repos/$SC/commits?sha=main&until=2026-09-10T00:00:00Z&per_page=1" "[{\"sha\":\"$A\"}]"
expect_decide "coord:landed with no land comment (empty-diff land) -> A from main at closed_at -> FOLLOW" FOLLOW ""

setup_base open
gh_fx "repos/$SC/pulls/50" '{"number":50,"state":"open","merged_at":null,"base":{"ref":"main"},"labels":[]}'
expect_decide "partner still open -> SKIP" SKIP partner-not-landed

setup_base abandoned
gh_fx "repos/$SC/pulls/50" '{"number":50,"state":"closed","merged_at":null,"closed_at":"2026-09-10T00:00:00Z","base":{"ref":"main"},"labels":[]}'
expect_decide "partner closed, unlabelled, unmerged -> SKIP" SKIP partner-abandoned

setup_base offmain
gh_fx "repos/$SC/pulls/50" "{\"number\":50,\"state\":\"closed\",\"merged_at\":\"2026-09-10T00:00:00Z\",\"merge_commit_sha\":\"$A\",\"base\":{\"ref\":\"feature\"},\"labels\":[]}"
expect_decide "partner merged into a non-main base -> SKIP" SKIP partner-not-on-main

setup_base trailer
gh_fx "repos/$R/contents/.github/sibling-pins.conf?ref=$HEAD" "$(conf_json "$A")"
expect_decide "already followed (trailer commit pinned A) -> SKIP contained" SKIP contained

setup_base atmain
gh_fx "repos/$R/contents/.github/sibling-pins.conf?ref=$HEAD" "$(conf_json "$K")"
gh_fx "repos/$SC/compare/$A...$K" '{"status":"ahead"}'
expect_decide "pin already at schemas main's head -> SKIP contained" SKIP contained

setup_base handrepin
gh_fx "repos/$R/contents/.github/sibling-pins.conf?ref=$HEAD" "$(conf_json "$NEWER")"
gh_fx "repos/$SC/compare/$A...$NEWER" '{"status":"ahead"}'
expect_decide "hand re-pin already contains A (trailer squashed away) -> SKIP contained" SKIP contained

setup_base mergeonly
gh_fx "repos/$R/contents/.github/sibling-pins.conf?ref=$MAIN" "$(conf_json "$M2")"
gh_fx "repos/$SC/compare/$A...$M2" '{"status":"ahead"}'
expect_decide "PR head pin lacks A but runner main's pin contains it -> FOLLOW" FOLLOW ""
expect_field "  ...mode MERGE_ONLY" .mode MERGE_ONLY

setup_base claimbranch
coord_fx branch_name agent/x 200 '{"holder":{"machine_id":"m"},"last_release":null}'
expect_decide "live branch_name claim -> SKIP" SKIP live-claim:branch_name

setup_base claimciwait
coord_fx ci_wait "qontinui-runner#100" 200 '{"holder":{"machine_id":"m"},"last_release":null}'
expect_decide "live ci_wait claim (<repo-name>#<n>) -> SKIP" SKIP live-claim:ci_wait

setup_base claimciwaitfull
coord_fx ci_wait "qontinui/qontinui-runner#100" 200 '{"holder":{"machine_id":"m"},"last_release":null}'
expect_decide "live ci_wait claim (<owner>/<name>#<n>) -> SKIP" SKIP live-claim:ci_wait

setup_base claimrepobranch
coord_fx repo_branch "qontinui-runner:agent/x" 200 '{"holder":{"machine_id":"m"},"last_release":null}'
expect_decide "live repo_branch claim -> SKIP" SKIP live-claim:repo_branch

setup_base claim500
coord_fx branch_name agent/x 500 'oops'
expect_decide "claim read non-200 -> UNKNOWN" UNKNOWN "claim read"

setup_base claimjunk
coord_fx branch_name agent/x 200 'not json'
expect_decide "claim read unparseable -> UNKNOWN" UNKNOWN "claim read"

setup_base claim401
coord_fx ci_wait "qontinui-runner#100" 401 '{"error":"unauthorized"}'
expect_decide "claim read 401 (auth flag armed) -> UNKNOWN" UNKNOWN "claim read"
grep -q COORD_CLAIMS_READ_TOKEN "$FX/stderr" && ok "  ...names COORD_CLAIMS_READ_TOKEN" || bad "  ...names COORD_CLAIMS_READ_TOKEN" "$(cat "$FX/stderr")"

setup_base fork
gh_fx "repos/$R/pulls/100" "{\"number\":100,\"state\":\"open\",\"head\":{\"sha\":\"$HEAD\",\"ref\":\"agent/x\",\"repo\":{\"full_name\":\"someone/qontinui-runner\"}},\"labels\":[]}"
expect_decide "fork PR -> SKIP" SKIP fork-pr

setup_base nopin
gh_fx "repos/$R/contents/.github/sibling-pins.conf?ref=$MAIN" "$(jq -cn --arg c "$(printf 'qontinui/ui-bridge %s\n' "$NEWER" | base64 -w0)" '{content:$c}')"
expect_decide "runner main carries no schemas pin (before Phase 3) -> SKIP" SKIP no-pin

setup_base noconf
gh_fx "repos/$R/contents/.github/sibling-pins.conf?ref=$MAIN" "__404__"
expect_decide "runner main has no pin file at all -> SKIP" SKIP no-pin

setup_base kill
KILL=off expect_decide "kill switch SCHEMAS_PAIR_FOLLOW=off -> SKIP" SKIP kill-switch

setup_base trailing
gh_fx "repos/$R/issues/100/events?per_page=100" '[]'
gh_fx "repos/$R/pulls/100" "{\"number\":100,\"state\":\"open\",\"head\":{\"sha\":\"$HEAD\",\"ref\":\"agent/x\",\"repo\":{\"full_name\":\"$R\"}},\"base\":{\"ref\":\"main\"},\"labels\":[{\"name\":\"coord:upstream-of=qontinui-schemas#50\"}]}"
expect_decide "runner-leads (trailing) declaration -> SKIP" SKIP trailing-declaration

setup_base nodecl
gh_fx "repos/$R/issues/100/events?per_page=100" '[]'
expect_decide "no declaration pairing the two -> SKIP" SKIP no-declaration

setup_base unresolvable
gh_fx "repos/$SC/issues/50/comments?per_page=100" '[]'
gh_fx "repos/$SC/commits?sha=main&until=2026-09-10T00:00:00Z&per_page=1" '[]'
expect_decide "land commit A unresolvable -> UNKNOWN" UNKNOWN land-commit-unresolvable

setup_base notonmain
gh_fx "repos/$SC/compare/$A...main" '{"status":"diverged"}'
expect_decide "resolved A is not on schemas main -> UNKNOWN" UNKNOWN land-commit-unresolvable

setup_base prreaderr
gh_fx "repos/$R/pulls/100" "__ERR__"
expect_decide "runner PR unreadable -> UNKNOWN" UNKNOWN "read $R#100"

setup_base refused
gh_fx "repos/$R/issues/100/comments?per_page=100" "[{\"user\":{\"login\":\"github-actions[bot]\"},\"body\":\"needs a human <!-- schemas-pair-follow:refused a=$A -->\"}]"
expect_decide "already refused for this land commit -> SKIP (no re-refusal every cycle)" SKIP refused-awaiting-human

setup_base refusedother
gh_fx "repos/$R/issues/100/comments?per_page=100" "[{\"user\":{\"login\":\"github-actions[bot]\"},\"body\":\"<!-- schemas-pair-follow:refused a=$K -->\"},{\"user\":{\"login\":\"an-author\"},\"body\":\"<!-- schemas-pair-follow:refused a=$A -->\"}]"
expect_decide "refusal marker for another commit, or not posted by the bot -> still FOLLOW" FOLLOW ""

setup_base stacked
gh_fx "repos/$R/pulls/100" "{\"number\":100,\"state\":\"open\",\"head\":{\"sha\":\"$HEAD\",\"ref\":\"agent/x\",\"repo\":{\"full_name\":\"$R\"}},\"base\":{\"ref\":\"agent/parent\"},\"labels\":[]}"
expect_decide "stacked runner PR (base is not main) -> SKIP" SKIP pr-not-on-main

setup_base withdrawn
gh_fx "repos/$R/issues/100/events?per_page=100" '[{"event":"labeled","label":{"name":"coord:downstream-of=qontinui-schemas#50"}},{"event":"unlabeled","actor":{"login":"an-author"},"label":{"name":"coord:downstream-of=qontinui-schemas#50"}}]'
expect_decide "label withdrawn by the author (not a coord strip) -> SKIP" SKIP no-declaration

echo "# scan"
new_fx scan
gh_fx "repos/$R/pulls?state=open&per_page=100&page=1" '[{"number":100,"created_at":"2026-09-01T00:00:00Z","labels":[]},{"number":101,"created_at":"2026-09-05T00:00:00Z","labels":[{"name":"coord:downstream-of=qontinui/qontinui-schemas#60"}]},{"number":102,"created_at":"2026-09-06T00:00:00Z","labels":[]},{"number":103,"created_at":"2026-09-06T00:00:00Z","labels":[]}]'
gh_fx "repos/$R/issues/100/events?per_page=100" '[]'
gh_fx "repos/$R/issues/101/events?per_page=100" '[]'
gh_fx "repos/$R/issues/102/events?per_page=100" '[{"event":"unlabeled","actor":{"login":"qontinui-merge-orchestrator[bot]"},"label":{"name":"coord:downstream-of=qontinui-schemas#70"}}]'
gh_fx "repos/$R/issues/103/events?per_page=100" '[{"event":"unlabeled","actor":{"login":"an-author"},"label":{"name":"coord:downstream-of=qontinui-schemas#80"}}]'
gh_fx "repos/$SC/pulls?state=closed&sort=updated&direction=desc&per_page=100&page=1" '[{"number":52,"updated_at":"2026-09-11T00:00:00Z","labels":[{"name":"coord:upstream-of=qontinui-runner#999"}]},{"number":50,"updated_at":"2026-09-10T00:00:00Z","labels":[{"name":"coord:landed"},{"name":"coord:upstream-of=qontinui-runner#100"}]},{"number":51,"updated_at":"2026-08-01T00:00:00Z","labels":[{"name":"coord:upstream-of=qontinui-runner#100"}]}]'
got="$(SPF_FIXTURES="$FX" bash "$SUT" scan 2>"$FX/stderr" | tr '\n' ';')"
[ "$got" = "100 50;101 60;102 70;" ] && ok "scan: form 1 in window + form 2 labelled + form 2 stripped by coord; non-open, pre-window and author-withdrawn excluded" || bad "scan candidates" "got '$got'; stderr: $(cat "$FX/stderr")"
got="$(SPF_FIXTURES="$FX" SPF_KILL_SWITCH=off bash "$SUT" scan 2>/dev/null | tr '\n' ';')"
[ -z "$got" ] && ok "scan: kill switch -> no candidates" || bad "scan kill switch" "got '$got'"
gh_fx "repos/$R/pulls?state=open&per_page=100&page=1" "__ERR__"
if SPF_FIXTURES="$FX" bash "$SUT" scan >/dev/null 2>&1; then bad "scan: unreadable PR list must fail (UNKNOWN)"; else ok "scan: unreadable PR list fails red"; fi

echo "# compute + push (local git)"

# git_setup NAME VARIANT
#   plain     : agent/x adds feature.txt; main adds a src.txt line; both pins OLD
#   pinmoved  : main ALSO moved the schemas pin (to NEWER) and the ui-bridge pin after agent/x branched
#   conflict  : agent/x and main edit the same src.txt line differently
#   mainpin   : main's pin is already M2, which contains A
git_setup() {
  G="$TMP/git-$1"
  rm -rf "$G"
  mkdir -p "$G"
  local w="$G/work"
  git init -q -b main "$w"
  git -C "$w" config user.email t@t
  git -C "$w" config user.name t
  mkdir -p "$w/.github"
  printf '# pins\nqontinui/ui-bridge %s\nqontinui/qontinui-schemas %s\n' "$NEWER" "$OLD" >"$w/.github/sibling-pins.conf"
  printf 'version = 4\n\n[[package]]\nname = "qontinui-types"\nversion = "1.9.0"\n\n[[package]]\nname = "serde"\nversion = "1.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "abc"\n' >"$w/Cargo.lock"
  printf 'line one\n' >"$w/src.txt"
  git -C "$w" add -A && git -C "$w" commit -qm base
  git -C "$w" checkout -q -b agent/x
  if [ "$2" = conflict ]; then printf 'branch edit\n' >"$w/src.txt"; else printf 'feature\n' >"$w/feature.txt"; fi
  git -C "$w" add -A && git -C "$w" commit -qm feature
  GHEAD="$(git -C "$w" rev-parse HEAD)"
  git -C "$w" checkout -q main
  case "$2" in
    conflict) printf 'main edit\n' >"$w/src.txt" ;;
    pinmoved) printf 'line two\n' >>"$w/src.txt"
      printf '# pins\nqontinui/ui-bridge %s\nqontinui/qontinui-schemas %s\n' "$K" "$NEWER" >"$w/.github/sibling-pins.conf" ;;
    mainpin) printf '# pins\nqontinui/ui-bridge %s\nqontinui/qontinui-schemas %s\n' "$NEWER" "$M2" >"$w/.github/sibling-pins.conf" ;;
    *) printf 'line two\n' >>"$w/src.txt" ;;
  esac
  git -C "$w" add -A && git -C "$w" commit -qm "main moves"
  GMAIN="$(git -C "$w" rev-parse HEAD)"
  git init -q --bare "$G/remote.git"
  git -C "$G/remote.git" config uploadpack.allowAnySHA1InWant true
  git -C "$G/remote.git" symbolic-ref HEAD refs/heads/main
  git -C "$w" push -q "$G/remote.git" main agent/x
  git clone -q "$G/remote.git" "$G/compute"
  git -C "$G/compute" checkout -q --detach "$GHEAD"
  jq -n --arg a "$A" --arg h "$GHEAD" --arg m "$GMAIN" \
    '{pr:100, partner:50, action:"FOLLOW", a:$a, head_sha:$h, main_sha:$m, head_ref:"agent/x", mode:"FOLLOW", label_present:false}' >"$G/decide.json"
  new_fx "git-$1"
  gh_fx "repos/$SC/compare/$A...$OLD" '{"status":"behind"}'
  gh_fx "repos/$SC/compare/$A...$NEWER" '{"status":"behind"}'
  gh_fx "repos/$SC/compare/$A...$M2" '{"status":"ahead"}'
  gh_fx "repos/$R/issues/100/comments?per_page=100" '[]'
  gh_fx "repos/$R/pulls/100" "{\"number\":100,\"state\":\"open\",\"head\":{\"sha\":\"$GHEAD\",\"ref\":\"agent/x\",\"repo\":{\"full_name\":\"$R\"}},\"base\":{\"ref\":\"main\"},\"labels\":[]}"
}
spf_git() { SPF_FIXTURES="$FX" SPF_LOCK_CMD="${LOCK_CMD:-sed -i 's/^version = \"1.9.0\"$/version = \"2.0.0\"/' Cargo.lock}" SPF_FETCH_URL="file://$G/remote.git" SPF_PUSH_URL="file://$G/remote.git" SPF_FETCH_FILTER="" bash "$SUT" "$@"; }
remote_tip() { git -C "$G/remote.git" rev-parse refs/heads/agent/x; }

# FOLLOW, end to end.
git_setup follow plain
mp="$(spf_git merge-and-pin "$G/compute" "$G/decide.json" 2>"$FX/stderr")"
grep -qx "MODE=FOLLOW" <<<"$mp" && ok "merge-and-pin: stale pin -> MODE=FOLLOW" || bad "merge-and-pin FOLLOW" "$mp $(cat "$FX/stderr")"
lc="$(spf_git lock-and-commit "$G/compute" "$G/decide.json" "$G/out" FOLLOW 2>"$FX/stderr")" && ok "lock-and-commit FOLLOW" || bad "lock-and-commit FOLLOW" "$(cat "$FX/stderr")"
pr="$(spf_git push "$G/decide.json" "$G/out" "$G/verify" 2>"$FX/stderr")"
case "$pr" in RESULT=PUSHED*) ok "push: verified bundle pushed with lease" ;; *) bad "push FOLLOW" "$pr $(cat "$FX/stderr")" ;; esac
tip="$(remote_tip)"
[ "$(git -C "$G/remote.git" show "$tip:.github/sibling-pins.conf" | awk '$1=="qontinui/qontinui-schemas"{print $2}')" = "$A" ] && ok "  ...remote branch now pins A" || bad "  ...remote pin"
git -C "$G/remote.git" show "$tip:Cargo.lock" | grep -qx 'version = "2.0.0"' && ok "  ...Cargo.lock moved in the same commit" || bad "  ...lock"
git -C "$G/remote.git" log -1 --format=%B "$tip" | grep -qx "Schemas-Pair-Follow: $SC#50" && ok "  ...pin commit carries the provenance trailer" || bad "  ...trailer"
[ "$(git -C "$G/remote.git" rev-list --parents -n 1 "$tip^" | wc -w)" -eq 3 ] && ok "  ...main was merged first" || bad "  ...merge commit"
grep -q "^POST repos/$R/issues/100/comments" "$FX/writes.log" 2>/dev/null && ok "  ...one explanatory comment" || bad "  ...comment"

# Idempotence at the script level: a second decide sees the pushed pin as contained.
setup_base idem
gh_fx "repos/$R/contents/.github/sibling-pins.conf?ref=$HEAD" "$(conf_json "$A")"
expect_decide "re-run after the push -> SKIP contained (no re-push)" SKIP contained

# main's pin moved since the PR branched: the merge takes main's line, the pin commit rewrites it, and a rebase onto main is clean.
git_setup pinmoved pinmoved
mp="$(spf_git merge-and-pin "$G/compute" "$G/decide.json" 2>"$FX/stderr")"
grep -qx "MODE=FOLLOW" <<<"$mp" && ok "pin moved on main -> merge then FOLLOW" || bad "pinmoved merge-and-pin" "$mp $(cat "$FX/stderr")"
spf_git lock-and-commit "$G/compute" "$G/decide.json" "$G/out" FOLLOW >/dev/null 2>"$FX/stderr" || bad "pinmoved lock-and-commit" "$(cat "$FX/stderr")"
pr="$(spf_git push "$G/decide.json" "$G/out" "$G/verify" 2>"$FX/stderr")"
case "$pr" in RESULT=PUSHED*) ok "  ...pushed" ;; *) bad "pinmoved push" "$pr $(cat "$FX/stderr")" ;; esac
git clone -q "$G/remote.git" "$G/rebase" && git -C "$G/rebase" config user.email t@t && git -C "$G/rebase" config user.name t
git -C "$G/rebase" checkout -q agent/x
if git -C "$G/rebase" rebase -q origin/main >/dev/null 2>&1; then ok "  ...the followed branch rebases cleanly onto main"; else bad "  ...rebase onto main conflicts"; fi
[ "$(awk '$1=="qontinui/ui-bridge"{print $2}' "$G/rebase/.github/sibling-pins.conf")" = "$K" ] && ok "  ...main's other pin moves survive" || bad "  ...ui-bridge pin lost"

# Merge conflict: refuse, nothing committed.
git_setup conflict conflict
mp="$(spf_git merge-and-pin "$G/compute" "$G/decide.json" 2>"$FX/stderr")"
grep -qx "REASON=merge-conflict" <<<"$mp" && ok "merge of main conflicts -> SKIP merge-conflict" || bad "conflict" "$mp $(cat "$FX/stderr")"
[ "$(git -C "$G/compute" rev-parse HEAD)" = "$GHEAD" ] && [ -z "$(git -C "$G/compute" status --porcelain)" ] && ok "  ...worktree left clean at the head" || bad "  ...worktree not clean"

# main's pin already contains A: merge-only.
git_setup mergeonly mainpin
mp="$(spf_git merge-and-pin "$G/compute" "$G/decide.json" 2>"$FX/stderr")"
grep -qx "MODE=MERGE_ONLY" <<<"$mp" && ok "main's pin contains A -> MODE=MERGE_ONLY" || bad "merge-only" "$mp $(cat "$FX/stderr")"
spf_git lock-and-commit "$G/compute" "$G/decide.json" "$G/out" MERGE_ONLY >/dev/null 2>"$FX/stderr" || bad "merge-only lock-and-commit" "$(cat "$FX/stderr")"
pr="$(spf_git push "$G/decide.json" "$G/out" "$G/verify" 2>"$FX/stderr")"
case "$pr" in RESULT=PUSHED*) ok "  ...merge commit alone pushed" ;; *) bad "merge-only push" "$pr $(cat "$FX/stderr")" ;; esac

# Head moved before the push: ABORT, remote untouched.
git_setup moved plain
spf_git merge-and-pin "$G/compute" "$G/decide.json" >/dev/null 2>&1
spf_git lock-and-commit "$G/compute" "$G/decide.json" "$G/out" FOLLOW >/dev/null 2>&1
gh_fx "repos/$R/pulls/100" "{\"number\":100,\"state\":\"open\",\"head\":{\"sha\":\"$HEAD\",\"ref\":\"agent/x\",\"repo\":{\"full_name\":\"$R\"}},\"base\":{\"ref\":\"main\"},\"labels\":[]}"
before="$(remote_tip)"
pr="$(spf_git push "$G/decide.json" "$G/out" "$G/verify" 2>"$FX/stderr")"
case "$pr" in RESULT=ABORT\ head-moved*) ok "head moved before push -> ABORT" ;; *) bad "head moved" "$pr $(cat "$FX/stderr")" ;; esac
[ "$(remote_tip)" = "$before" ] && ok "  ...nothing pushed" || bad "  ...remote moved"

# The lease: the API still shows the old head but the branch actually moved -> refused, not clobbered.
git_setup lease plain
spf_git merge-and-pin "$G/compute" "$G/decide.json" >/dev/null 2>&1
spf_git lock-and-commit "$G/compute" "$G/decide.json" "$G/out" FOLLOW >/dev/null 2>&1
git -C "$G/work" checkout -q agent/x && printf 'author pushed\n' >"$G/work/author.txt" && git -C "$G/work" add -A && git -C "$G/work" commit -qm author && git -C "$G/work" push -q "$G/remote.git" agent/x
authored="$(remote_tip)"
pr="$(spf_git push "$G/decide.json" "$G/out" "$G/verify" 2>"$FX/stderr")"
case "$pr" in RESULT=ABORT\ lease-rejected*) ok "branch moved under a stale API read -> lease rejects -> ABORT" ;; *) bad "lease" "$pr $(cat "$FX/stderr")" ;; esac
[ "$(remote_tip)" = "$authored" ] && ok "  ...the author's commit survives" || bad "  ...author commit clobbered"

# A still-present form-2 label is removed before the push.
git_setup label plain
spf_git merge-and-pin "$G/compute" "$G/decide.json" >/dev/null 2>&1
spf_git lock-and-commit "$G/compute" "$G/decide.json" "$G/out" FOLLOW >/dev/null 2>&1
gh_fx "repos/$R/pulls/100" "{\"number\":100,\"state\":\"open\",\"head\":{\"sha\":\"$GHEAD\",\"ref\":\"agent/x\",\"repo\":{\"full_name\":\"$R\"}},\"base\":{\"ref\":\"main\"},\"labels\":[{\"name\":\"coord:downstream-of=qontinui-schemas#50\"},{\"name\":\"unrelated\"}]}"
pr="$(spf_git push "$G/decide.json" "$G/out" "$G/verify" 2>"$FX/stderr")"
grep -q "^DELETE repos/$R/issues/100/labels/coord%3Adownstream-of%3Dqontinui-schemas%2350" "$FX/writes.log" 2>/dev/null && ok "form-2 label still present -> removed before the push" || bad "label removal" "$(cat "$FX/writes.log" 2>/dev/null) $pr"
grep -q "labels/unrelated" "$FX/writes.log" 2>/dev/null && bad "  ...an unrelated label was removed" || ok "  ...unrelated labels untouched"

# A bundle touching anything beyond the conf and Cargo.lock is refused by the push job.
git_setup scope plain
spf_git merge-and-pin "$G/compute" "$G/decide.json" >/dev/null 2>&1
spf_git lock-and-commit "$G/compute" "$G/decide.json" "$G/out" FOLLOW >/dev/null 2>&1
printf 'smuggled\n' >>"$G/compute/feature.txt"
git -C "$G/compute" -c user.email=t@t -c user.name=t commit -qam "Schemas-Pair-Follow: $SC#50"
git -C "$G/compute" bundle create -q "$G/out/pair-follow.bundle" "$GHEAD..HEAD" 2>/dev/null || git -C "$G/compute" bundle create "$G/out/pair-follow.bundle" "$GHEAD..HEAD" >/dev/null 2>&1
jq --arg n "$(git -C "$G/compute" rev-parse HEAD)" '.new_sha = $n' "$G/out/meta.json" >"$G/out/m2" && mv "$G/out/m2" "$G/out/meta.json"
before="$(remote_tip)"
if spf_git push "$G/decide.json" "$G/out" "$G/verify" >/dev/null 2>"$FX/stderr"; then bad "extra file in the patch must be refused"; else grep -q patch-scope "$FX/stderr" && ok "patch touching a file beyond the conf and Cargo.lock -> refused" || bad "scope refusal reason" "$(cat "$FX/stderr")"; fi
[ "$(remote_tip)" = "$before" ] && ok "  ...nothing pushed" || bad "  ...remote moved"

# A forged merge commit (not git's own merge of head and main) is refused.
git_setup forged plain
spf_git merge-and-pin "$G/compute" "$G/decide.json" >/dev/null 2>&1
spf_git lock-and-commit "$G/compute" "$G/decide.json" "$G/out" FOLLOW >/dev/null 2>&1
git -C "$G/compute" checkout -q --detach "$GHEAD"
printf 'evil\n' >"$G/compute/evil.txt" && git -C "$G/compute" add evil.txt
tree="$(git -C "$G/compute" write-tree)"
fm="$(git -C "$G/compute" -c user.email=t@t -c user.name=t commit-tree "$tree" -p "$GHEAD" -p "$GMAIN" -m merge)"
git -C "$G/compute" reset -q --hard "$fm"
printf '# pins\nqontinui/ui-bridge %s\nqontinui/qontinui-schemas %s\n' "$NEWER" "$A" >"$G/compute/.github/sibling-pins.conf"
git -C "$G/compute" -c user.email=t@t -c user.name=t commit -qam "pin" -m "Schemas-Pair-Follow: $SC#50"
git -C "$G/compute" bundle create -q "$G/out/pair-follow.bundle" "$GHEAD..HEAD" 2>/dev/null || git -C "$G/compute" bundle create "$G/out/pair-follow.bundle" "$GHEAD..HEAD" >/dev/null 2>&1
jq --arg n "$(git -C "$G/compute" rev-parse HEAD)" '.new_sha = $n' "$G/out/meta.json" >"$G/out/m2" && mv "$G/out/m2" "$G/out/meta.json"
if spf_git push "$G/decide.json" "$G/out" "$G/verify" >/dev/null 2>"$FX/stderr"; then bad "forged merge commit must be refused"; else grep -q "not git's merge" "$FX/stderr" && ok "forged merge commit (extra file hidden in the merge) -> refused" || bad "forged merge reason" "$(cat "$FX/stderr")"; fi

# The artifact is untrusted: a forged meta.json cannot redirect the push.
git_setup forgedmeta plain
spf_git merge-and-pin "$G/compute" "$G/decide.json" >/dev/null 2>&1
spf_git lock-and-commit "$G/compute" "$G/decide.json" "$G/out" FOLLOW >/dev/null 2>&1
jq '. + {pr: 999, head_ref: "main", head_sha: "'"$GMAIN"'", main_sha: "'"$GHEAD"'", mode: "MERGE_ONLY"}' "$G/out/meta.json" >"$G/out/m2" && mv "$G/out/m2" "$G/out/meta.json"
mainbefore="$(git -C "$G/remote.git" rev-parse refs/heads/main)"
pr="$(spf_git push "$G/decide.json" "$G/out" "$G/verify" 2>"$FX/stderr")"
case "$pr" in RESULT=PUSHED*) ok "forged artifact meta (pr/head_ref/head/main/mode) is ignored; targets come from the plan item" ;; *) bad "forged meta" "$pr $(cat "$FX/stderr")" ;; esac
[ "$(git -C "$G/remote.git" rev-parse refs/heads/main)" = "$mainbefore" ] && ok "  ...runner main untouched" || bad "  ...main moved"

# A bundle that does not descend from the trusted head (built on main) is refused.
git_setup offhead plain
git -C "$G/compute" checkout -q --detach "$GMAIN"
printf '# pins\nqontinui/ui-bridge %s\nqontinui/qontinui-schemas %s\n' "$NEWER" "$A" >"$G/compute/.github/sibling-pins.conf"
git -C "$G/compute" -c user.email=t@t -c user.name=t commit -qam pin -m "Schemas-Pair-Follow: $SC#50"
mkdir -p "$G/out" && git -C "$G/compute" bundle create -q "$G/out/pair-follow.bundle" "$GMAIN..HEAD" 2>/dev/null || git -C "$G/compute" bundle create "$G/out/pair-follow.bundle" "$GMAIN..HEAD" >/dev/null 2>&1
jq -n --arg n "$(git -C "$G/compute" rev-parse HEAD)" '{new_sha:$n}' >"$G/out/meta.json"
if spf_git push "$G/decide.json" "$G/out" "$G/verify" >/dev/null 2>"$FX/stderr"; then bad "bundle not descending from the trusted head must be refused"; else grep -q patch-scope "$FX/stderr" && ok "bundle not descending from the trusted head -> refused" || bad "offhead reason" "$(cat "$FX/stderr")"; fi

# A pin commit that pins anything other than exactly A is refused.
git_setup wrongpin plain
LOCK_CMD="printf '# pins\nqontinui/ui-bridge %s\nqontinui/qontinui-schemas %s\n' $NEWER $K > .github/sibling-pins.conf" spf_git merge-and-pin "$G/compute" "$G/decide.json" >/dev/null 2>&1
LOCK_CMD="printf '# pins\nqontinui/ui-bridge %s\nqontinui/qontinui-schemas %s\n' $NEWER $K > .github/sibling-pins.conf" spf_git lock-and-commit "$G/compute" "$G/decide.json" "$G/out" FOLLOW >/dev/null 2>&1
if spf_git push "$G/decide.json" "$G/out" "$G/verify" >/dev/null 2>"$FX/stderr"; then bad "a pin other than A must be refused"; else grep -q "exactly $A" "$FX/stderr" && ok "pin commit setting a pin other than A -> refused" || bad "wrong pin reason" "$(cat "$FX/stderr")"; fi

# A Cargo.lock change to a registry entry is refused.
git_setup reglock plain
spf_git merge-and-pin "$G/compute" "$G/decide.json" >/dev/null 2>&1
LOCK_CMD="sed -i 's/^checksum = \"abc\"$/checksum = \"evil\"/' Cargo.lock" spf_git lock-and-commit "$G/compute" "$G/decide.json" "$G/out" FOLLOW >/dev/null 2>&1
before="$(remote_tip)"
pr="$(spf_git push "$G/decide.json" "$G/out" "$G/verify" 2>"$FX/stderr")"; rc=$?
[ "$rc" -eq 0 ] && [ "$pr" = "RESULT=REFUSED lock-registry-change" ] && [ "$(remote_tip)" = "$before" ] && ok "Cargo.lock registry entry changed -> REFUSED (named, not red), nothing pushed" || bad "reglock" "rc=$rc $pr $(cat "$FX/stderr")"
grep -c "schemas-pair-follow:refused a=$A" "$FX/writes.log" 2>/dev/null | grep -qx 1 && ok "  ...the PR is told once, with the marker decide honours" || bad "  ...refusal comment" "$(cat "$FX/writes.log" 2>/dev/null)"
gh_fx "repos/$R/issues/100/comments?per_page=100" "[{\"user\":{\"login\":\"github-actions[bot]\"},\"body\":\"x <!-- schemas-pair-follow:refused a=$A -->\"}]"
spf_git push "$G/decide.json" "$G/out" "$G/verify" >/dev/null 2>&1
[ "$(grep -c "schemas-pair-follow:refused a=$A" "$FX/writes.log")" -eq 1 ] && ok "  ...a second refusal does not comment again" || bad "  ...duplicate refusal comment"

# The PR changed since the plan (closed, retargeted, ref changed) -> ABORT.
for variant in closed retarget; do
  git_setup "pr$variant" plain
  spf_git merge-and-pin "$G/compute" "$G/decide.json" >/dev/null 2>&1
  spf_git lock-and-commit "$G/compute" "$G/decide.json" "$G/out" FOLLOW >/dev/null 2>&1
  if [ "$variant" = closed ]; then st=closed; base=main; else st=open; base=agent/parent; fi
  gh_fx "repos/$R/pulls/100" "{\"number\":100,\"state\":\"$st\",\"head\":{\"sha\":\"$GHEAD\",\"ref\":\"agent/x\",\"repo\":{\"full_name\":\"$R\"}},\"base\":{\"ref\":\"$base\"},\"labels\":[]}"
  before="$(remote_tip)"
  pr="$(spf_git push "$G/decide.json" "$G/out" "$G/verify" 2>"$FX/stderr")"
  [ "$pr" = "RESULT=ABORT pr-changed" ] && [ "$(remote_tip)" = "$before" ] && ok "PR $variant since the plan -> ABORT, nothing pushed" || bad "pr $variant" "$pr $(cat "$FX/stderr")"
done

# A claim taken between plan and push -> ABORT at push time.
git_setup claimatpush plain
spf_git merge-and-pin "$G/compute" "$G/decide.json" >/dev/null 2>&1
spf_git lock-and-commit "$G/compute" "$G/decide.json" "$G/out" FOLLOW >/dev/null 2>&1
coord_fx repo_branch "qontinui-runner:agent/x" 200 '{"holder":{"machine_id":"m"},"last_release":null}'
before="$(remote_tip)"
pr="$(spf_git push "$G/decide.json" "$G/out" "$G/verify" 2>"$FX/stderr")"
[ "$pr" = "RESULT=ABORT live-claim:repo_branch" ] && [ "$(remote_tip)" = "$before" ] && ok "claim taken after the plan -> ABORT at push, nothing pushed" || bad "claim at push" "$pr $(cat "$FX/stderr")"
coord_fx branch_name agent/x 503 'down'
pr="$(spf_git push "$G/decide.json" "$G/out" "$G/verify" 2>"$FX/stderr")"; rc=$?
[ "$rc" -ne 0 ] && ok "claim re-read unknown at push -> red" || bad "claim unknown at push" "$pr"

# An unsafe head ref in the plan item is refused before anything else.
git_setup badref plain
jq '.head_ref = "-delete:main"' "$G/decide.json" >"$G/d2" && mv "$G/d2" "$G/decide.json"
mkdir -p "$G/out" && jq -n --arg n "$GHEAD" '{new_sha:$n}' >"$G/out/meta.json"
if spf_git push "$G/decide.json" "$G/out" "$G/verify" >/dev/null 2>"$FX/stderr"; then bad "unsafe head ref must be refused"; else grep -q "not a safe branch name" "$FX/stderr" && ok "unsafe head ref in the plan item -> refused" || bad "badref reason" "$(cat "$FX/stderr")"; fi

# Lease rejected after the label was removed -> the label is put back.
git_setup restore plain
spf_git merge-and-pin "$G/compute" "$G/decide.json" >/dev/null 2>&1
spf_git lock-and-commit "$G/compute" "$G/decide.json" "$G/out" FOLLOW >/dev/null 2>&1
gh_fx "repos/$R/pulls/100" "{\"number\":100,\"state\":\"open\",\"head\":{\"sha\":\"$GHEAD\",\"ref\":\"agent/x\",\"repo\":{\"full_name\":\"$R\"}},\"base\":{\"ref\":\"main\"},\"labels\":[{\"name\":\"coord:downstream-of=qontinui-schemas#50\"}]}"
git -C "$G/work" checkout -q agent/x && printf 'author\n' >"$G/work/a2.txt" && git -C "$G/work" add -A && git -C "$G/work" commit -qm author2 && git -C "$G/work" push -q "$G/remote.git" agent/x
pr="$(spf_git push "$G/decide.json" "$G/out" "$G/verify" 2>"$FX/stderr")"
[ "$pr" = "RESULT=ABORT lease-rejected" ] && ok "lease rejected with the label removed -> ABORT" || bad "restore abort" "$pr $(cat "$FX/stderr")"
grep -q "^POST repos/$R/issues/100/labels -f labels\[\]=coord:downstream-of=qontinui-schemas#50" "$FX/writes.log" 2>/dev/null && ok "  ...the removed label is restored" || bad "  ...label not restored" "$(cat "$FX/writes.log" 2>/dev/null)"

# Kill switch re-checked at push time; no PAT on an https remote fails red.
git_setup pushkill plain
spf_git merge-and-pin "$G/compute" "$G/decide.json" >/dev/null 2>&1
spf_git lock-and-commit "$G/compute" "$G/decide.json" "$G/out" FOLLOW >/dev/null 2>&1
pr="$(SPF_KILL_SWITCH=off spf_git push "$G/decide.json" "$G/out" "$G/verify" 2>/dev/null)"
[ "$pr" = "RESULT=SKIP kill-switch" ] && ok "push: kill switch re-checked -> SKIP" || bad "push kill switch" "$pr"
if SPF_FIXTURES="$FX" SPF_FETCH_URL="file://$G/remote.git" SPF_FETCH_FILTER="" SPF_PUSH_TOKEN="" bash "$SUT" push "$G/decide.json" "$G/out" "$G/verify" >/dev/null 2>"$FX/stderr"; then
  bad "push without a PAT to GitHub must fail red"
else
  grep -q CLORINDE_AUTOCOMMIT_TOKEN "$FX/stderr" && ok "push: no PAT for an https remote -> red, naming CLORINDE_AUTOCOMMIT_TOKEN" || bad "no-PAT message" "$(cat "$FX/stderr")"
fi

printf '\n%d passed, %d failed\n' "$passes" "$fails"
[ "$fails" -eq 0 ]
