#!/usr/bin/env bash
# Fixture tests for the runner-leads wait in .github/workflows/qontinui-types-drift.yml
# and its audit, .github/workflows/qontinui-types-drift-wait-audit.yml: the "Find a
# waiting runner-leads partner" step (API responses are files, no network), the
# "Re-check against the waiting partner" step (local git repos, stub generator),
# the "Decide the drift verdict" step (a decision table), the audit step (fixture
# API plus local git), and the structural properties that keep a drift red from
# failing OPEN. The step bodies are read OUT OF the workflows, so these tests
# exercise exactly what CI runs.
# Plan: qontinui-dev-notes plans/2026-11-05-runner-leads-schemas-pair-reds-runner-main-for-the-partners-land-latency.md
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "${here}/../../.." && pwd)"
wf="${root}/.github/workflows/qontinui-types-drift.yml"
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT
fails=0
ok()  { echo "ok   $*"; }
bad() { echo "FAIL $*"; fails=$((fails + 1)); }

python3 - "${wf}" "${work}" <<'PY'
import sys, yaml
wf, work = sys.argv[1], sys.argv[2]
steps = {s.get('name'): s for s in yaml.safe_load(open(wf))['jobs']['check-drift']['steps']}
open(f'{work}/partner.sh', 'w').write(steps['Find a waiting runner-leads partner']['run'])
open(f'{work}/decide.sh', 'w').write(steps['Decide the drift verdict']['run'])
open(f'{work}/recheck.sh', 'w').write(steps['Re-check against the waiting partner']['run'])
audit = yaml.safe_load(open(wf.replace('qontinui-types-drift.yml', 'qontinui-types-drift-wait-audit.yml')))
open(f'{work}/audit.sh', 'w').write(audit['jobs']['audit']['steps'][0]['run'])
PY
(cd "${work}" && python3 "${here}/make_fixtures.py")

# The 2026-11-05 episode's first push-run failure time.
now="$(date -u -d 2026-11-05T11:08:00Z +%s)"
run_partner() { # run_partner <script> <fixture>  -> sets RC and OUT
  local o; o="$(mktemp -p "${work}")"
  set +e
  mkdir -p "${work}/tmp-$2"
  (cd "${work}" && RUNNER_TEMP="${work}/tmp-$2" DRIFT_WAIT_TEST_FIXTURE_DIR="fx/$2" DRIFT_WAIT_TEST_NOW="${now}" DRIFT_WAIT_TEST_MAX_PAGES=3 GITHUB_OUTPUT="${o}" bash "$1" >"${work}/log-$2" 2>&1)
  RC=$?
  set -e
  OUT="$(tr '\n' ' ' <"${o}")"
}
expect() { # expect <fixture> <rc> <output-substring-or-empty> [<log substring>]
  run_partner partner.sh "$1"
  # A refusal must be the step's OWN refusal (its ::error line), not some
  # unrelated crash that happens to exit 1.
  local said=yes
  [ "$2" = 0 ] || grep -q '^::error title=runner-leads wait: ' "${work}/log-$1" || said=no
  [ -z "${4:-}" ] || grep -qF "$4" "${work}/log-$1" || said=no
  if [ "${RC}" = "$2" ] && [ "${said}" = yes ] && { [ -z "$3" ] || [[ "${OUT}" == *"$3"* ]]; }; then ok "partner $1 -> rc=${RC} ${3}"
  else bad "partner $1: want rc=$2 '$3' (own ::error: ${said}), got rc=${RC} '${OUT}': $(tail -2 "${work}/log-$1")"; fi
}
expect ok              0 "found=true pr=285 sha=$(printf 'a%.0s' {1..40}) merge_base=$(printf 'b%.0s' {1..40}) runner_pr=2546"
expect merged          0 "found=true pr=285"
expect samemtwice      0 "found=true pr=285"
# The audit's 72 h bound runs from the NEWEST strip of this partner.
run_partner partner.sh samemtwice
[[ "${OUT}" == *"stripped_at=2026-11-05T09:00:00Z"* ]] && ok "partner samemtwice -> newest strip time" || bad "partner samemtwice: stripped_at is not the newest strip: ${OUT}"
expect page2           0 "found=true pr=285"
expect human           0 "found=false"
expect notlanded       0 "found=false"
expect closedunlanded  0 "found=false"
expect mclosed         0 "found=false"
expect mbase           0 "found=false"
expect old             0 "found=false"
expect notpr           0 "found=false"
expect otherlabel      0 "found=false"
expect labeled         0 "found=false"
expect empty           0 "found=false"
expect runneropenlanded 0 "found=true pr=285"
expect runnerbase      0 "found=false"
expect mlandedlabel    0 "found=false"
expect codetouch       0 "found=false"
expect rusttouch       0 "found=false"
expect renamed         0 "found=true pr=285"
expect renamedfromscripts 0 "found=false"
expect cmp300          1 "" "the API ceiling"
expect cmp403          1 "" "answered HTTP 403"
expect cmpbehind       0 "found=false"
expect cmpidentical    0 "found=false"
expect cmpbehindfiles  0 "found=false"
expect cmpdiverged     0 "found=true pr=285"
expect cmpnostatus     1 "" "has no status"
expect notnewestfirst  1 "" "is not newest-first"
expect pagesoutoforder 1 "" "is not newest-first"
expect tomlonly        0 "found=false"
expect dotpath         0 "found=false" "changes more than bindings"
expect nullfilename    1 "" "a file entry this step cannot read"
expect nomergebase    1 "" "has no merge base"
expect badmergebase   1 "" "merge base is not a commit id"

# The file list the re-check lays over schemas main: exactly the compare's files.
run_partner partner.sh ok
if [[ "${OUT}" == *"files=${work}/tmp-ok/drift-wait-files-285.tsv"* ]] \
   && [ "$(cut -f1,2 "${work}/tmp-ok/drift-wait-files-285.tsv" | tr '\t\n' ' |')" = "added ts/src/generated/ManagedBy.d.ts|modified ts/src/generated/ScheduledTask.d.ts|modified src/qontinui_schemas/generated/__init__.py|modified codegen/pending-registration.toml|" ]; then
  ok "partner ok -> file list for the re-check"
else bad "partner ok: file list wrong: '${OUT}' / $(cat "${work}/tmp-ok/drift-wait-files-285.tsv" 2>&1)"; fi
expect two             1 "" "ambiguous"
expect api403          1 "" "answered HTTP 403"
expect runner500       1 "" "answered HTTP 500"
expect badsha          1 "" "is not a commit id"
# The page bound itself, not a fixture miss past it (MAX_PAGES=3 here).
expect full            1 "" "did not reach back 72 h"
expect notarray        1 "" "is not a JSON array"

# Mutations: each guard must be able to fail. Drop it, and the fixture it
# guards must flip to an absorbable partner.
mutate() { # mutate <sed-expr> <fixture>
  sed "$1" "${work}/partner.sh" >"${work}/mutant.sh"
  if cmp -s "${work}/partner.sh" "${work}/mutant.sh"; then bad "mutation '$1' changed nothing"; return; fi
  run_partner mutant.sh "$2"
  if [[ "${OUT}" == *"found=true"* ]]; then ok "mutation '$1' flips $2"; else bad "mutation '$1' did not flip $2 (rc=${RC} '${OUT}')"; fi
}
mutate 's/ and .actor == \$bot//' human
mutate 's/    | select((.at | fromdateiso8601) >= $cutoff)//' old
mutate 's/ and .ispr//' notpr
mutate 's/"open main"/"closed main"/' mclosed
mutate 's/ | .filename, (.previous_filename \/\/ empty) | / | .filename | /' renamedfromscripts
mutate 's/ or . == "codegen\/pending-registration.toml") and (test(/ or . == "codegen\/pending-registration.toml" or startswith("scripts\/")) and (test(/' codetouch
# The dot-segment arm of the same filter.
mutate 's/ and (test("(^|\/)\\\\.") | not))/)/' dotpath
mutate 's/behind|identical) echo/neverx) echo/; s/ahead|diverged) :/ahead|diverged|behind|identical) :/' cmpbehindfiles

# Decide: only a green re-check absorbs a drift red.
for r in success failure skipped cancelled ""; do
  set +e; (RECHECK="${r}" FOUND=x bash "${work}/decide.sh" >/dev/null 2>&1); rc=$?; set -e
  want=1; [ "${r}" = success ] && want=0
  if [ "${rc}" = "${want}" ]; then ok "decide recheck='${r}' -> ${rc}"; else bad "decide recheck='${r}': want ${want}, got ${rc}"; fi
done

# Re-check mechanics, against local git repos and stub generator/checker (no
# network, no cargo): it must reset to schemas main, apply exactly the partner's
# diff from its merge base for its listed generated files (added, modified,
# removed, renamed) three-way, commit that as HEAD, and absorb only when the
# regeneration matches that state. A listed path outside the generated bindings,
# or no binding path at all, is refused, so partner scripts never reach the tree.
recheck_case() { # recheck_case <name> <regen-matches: yes|no> <partner-landed: yes|no> <want-rc> [<script>] [<extra: none|symlink|diverged|mainahead|conflict|addonly|tomlonly|tomlrename|outside|dotdot|dotfile>]
  local script="${5:-recheck.sh}" extra="${6:-none}"
  local d="${work}/rc-$1"; mkdir -p "${d}"
  local up="${d}/upstream.git" sc="${d}/qontinui-schemas"
  git init -q --bare "${up}"
  git init -q "${d}/seed"; local g=(git -C "${d}/seed" -c user.name=t -c user.email=t@t)
  mkdir -p "${d}/seed/ts/src/generated" "${d}/seed/scripts"
  printf 'old\n' >"${d}/seed/ts/src/generated/Old.d.ts"
  printf 'inline ManagedBy\n' >"${d}/seed/ts/src/generated/ScheduledTask.d.ts"
  printf 'gone\n' >"${d}/seed/ts/src/generated/Gone.d.ts"
  printf 'other\n' >"${d}/seed/ts/src/generated/Other.d.ts"
  # A shared barrel every binding PR touches.
  printf '// barrel\nexport A\nexport B\nexport C\nexport D\n' >"${d}/seed/ts/src/generated/index.ts"
  printf 'main script\n' >"${d}/seed/scripts/note"
  mkdir -p "${d}/seed/codegen"; printf 'rows=0\n' >"${d}/seed/codegen/pending-registration.toml"
  # The stub checker: green iff the working tree equals HEAD, untracked included.
  printf '#!/usr/bin/env bash\n[ -z "$(git status --porcelain)" ]\n' >"${d}/seed/scripts/check-generated-drift.sh"
  "${g[@]}" add -A; "${g[@]}" commit -qm main
  local mainsha; mainsha="$("${g[@]}" rev-parse HEAD)"; local mb="${mainsha}"
  # The partner: adds ManagedBy, rewrites ScheduledTask, removes Gone, renames Old->New.
  "${g[@]}" checkout -q -b partner
  printf 'ManagedBy\n' >"${d}/seed/ts/src/generated/ManagedBy.d.ts"
  printf 'import ManagedBy\n' >"${d}/seed/ts/src/generated/ScheduledTask.d.ts"
  if [ "${extra}" = conflict ]; then sed -i 's/^export B$/export B-partner/' "${d}/seed/ts/src/generated/index.ts"
  else printf 'export ManagedBy\n' >>"${d}/seed/ts/src/generated/index.ts"; fi
  [ "${extra}" = addonly ] || { "${g[@]}" rm -q ts/src/generated/Gone.d.ts; "${g[@]}" mv ts/src/generated/Old.d.ts ts/src/generated/New.d.ts; }
  printf 'PARTNER SCRIPT — must never be laid over main\n' >"${d}/seed/scripts/note"
  printf 'rows=partner\n' >"${d}/seed/codegen/pending-registration.toml"
  [ "${extra}" != symlink ] || ln -s ../../../scripts/note "${d}/seed/ts/src/generated/Link.d.ts"
  "${g[@]}" add -A; "${g[@]}" commit -qm partner
  local psha; psha="$("${g[@]}" rev-parse HEAD)"; printf '%s' "${psha}" >"${d}/psha"
  # Schemas main moves on after the partner branched: another binding PR edits
  # the barrel (a different line, or — conflict — the same one).
  if [ "${extra}" = diverged ] || [ "${extra}" = mainahead ] || [ "${extra}" = conflict ]; then
    "${g[@]}" checkout -q -b moved "${mb}"
    if [ "${extra}" = conflict ]; then sed -i 's/^export B$/export B-main/' "${d}/seed/ts/src/generated/index.ts"
    else sed -i 's|^// barrel$|// barrel v2|' "${d}/seed/ts/src/generated/index.ts"; fi
    # ...and edits the declared-wait toml, which the re-check must NOT apply
    # (main's rows decide), so it cannot conflict either.
    printf 'rows=main\n' >"${d}/seed/codegen/pending-registration.toml"
    "${g[@]}" commit -qam moved; mainsha="$("${g[@]}" rev-parse HEAD)"
  fi
  # What runner main regenerates for the barrel: main's edits AND the partner's.
  if [ "${extra}" = diverged ]; then printf '// barrel v2\nexport A\nexport B\nexport C\nexport D\nexport ManagedBy\n' >"${d}/barrel.want"
  else printf '// barrel\nexport A\nexport B\nexport C\nexport D\nexport ManagedBy\n' >"${d}/barrel.want"; fi
  if [ "$3" = yes ]; then "${g[@]}" checkout -q -B landed "${psha}"; git -C "${d}/seed" push -q "${up}" "${psha}:refs/heads/main" "${psha}:refs/heads/partner"; mainsha="${psha}"
  else git -C "${d}/seed" push -q "${up}" "${mainsha}:refs/heads/main" "${psha}:refs/heads/partner"; fi
  git -C "${up}" config uploadpack.allowAnySHA1InWant true
  # The job's checkout of schemas MAIN, dirtied by the first regeneration.
  git init -q "${sc}"; git -C "${sc}" remote add origin "${up}"
  git -C "${sc}" fetch -q --depth 1 origin "${mainsha}"; git -C "${sc}" checkout -q FETCH_HEAD
  printf 'regen\n' >"${sc}/ts/src/generated/ManagedBy.d.ts"; printf 'dirty\n' >>"${sc}/ts/src/generated/ScheduledTask.d.ts"
  # ...including a tracked file and an untracked one the partner does NOT list.
  # The reset and the clean are what let the 3-way apply and the checker see
  # schemas main exactly; without either, the apply refuses (mut-noreset and
  # mut-noclean), which fails closed.
  printf 'dirty\n' >>"${sc}/ts/src/generated/Other.d.ts"; printf 'stray\n' >"${sc}/ts/src/generated/Stray.d.ts"
  # The stub generator writes what runner main would generate.
  mkdir -p "${d}/qontinui-runner/src-tauri/scripts"
  if [ "$2" = yes ]; then
    cat >"${d}/qontinui-runner/src-tauri/scripts/generate_types.sh" <<'GEN'
o=qontinui-schemas/ts/src/generated; printf 'ManagedBy\n' >$o/ManagedBy.d.ts; printf 'import ManagedBy\n' >$o/ScheduledTask.d.ts; printf 'old\n' >$o/New.d.ts; rm -f $o/Gone.d.ts $o/Old.d.ts; cp barrel.want $o/index.ts
GEN
  else
    cat >"${d}/qontinui-runner/src-tauri/scripts/generate_types.sh" <<'GEN'
o=qontinui-schemas/ts/src/generated; printf 'ManagedBy v2\n' >$o/ManagedBy.d.ts; printf 'import ManagedBy\n' >$o/ScheduledTask.d.ts; printf 'old\n' >$o/New.d.ts; rm -f $o/Gone.d.ts $o/Old.d.ts; cp barrel.want $o/index.ts
GEN
  fi
  printf 'added\tts/src/generated/ManagedBy.d.ts\t\nmodified\tts/src/generated/ScheduledTask.d.ts\t\nremoved\tts/src/generated/Gone.d.ts\t\nrenamed\tts/src/generated/New.d.ts\tts/src/generated/Old.d.ts\nmodified\tts/src/generated/index.ts\t\nmodified\tcodegen/pending-registration.toml\t\n' >"${d}/files.tsv"
  [ "${extra}" != addonly ] || printf 'added\tts/src/generated/ManagedBy.d.ts\t\nmodified\tts/src/generated/ScheduledTask.d.ts\t\nmodified\tts/src/generated/index.ts\t\n' >"${d}/files.tsv"
  [ "${extra}" != symlink ] || printf 'added\tts/src/generated/Link.d.ts\t\n' >>"${d}/files.tsv"
  [ "${extra}" != tomlonly ] || printf 'modified\tcodegen/pending-registration.toml\t\n' >"${d}/files.tsv"
  [ "${extra}" != tomlrename ] || printf 'renamed\tts/src/generated/Sneak.d.ts\tcodegen/pending-registration.toml\n' >>"${d}/files.tsv"
  [ "${extra}" != dotfile ] || printf 'added\tts/src/generated/.gitattributes\t\n' >>"${d}/files.tsv"
  [ "${extra}" != outside ] || printf 'modified\tscripts/note\t\n' >>"${d}/files.tsv"
  [ "${extra}" != dotdot ] || printf 'modified\tts/src/generated/../../../scripts/note\t\n' >>"${d}/files.tsv"
  set +e
  (cd "${d}" && PARTNER_SHA="${psha}" PARTNER_BASE="${mb}" PARTNER_PR=285 PARTNER_FILES="${d}/files.tsv" RUNNER_PR=2546 STRIPPED_AT=2026-11-05T08:15:55Z bash "${work}/${script}" >"${d}/log" 2>&1)
  local rc=$?
  set -e
  local note=""; [ "$(cat "${sc}/scripts/note")" = "main script" ] || { [ "$3" = yes ] || note=" (partner script leaked onto main!)"; }
  if [ "${rc}" = "$4" ] && [ -z "${note}" ]; then ok "recheck $1 -> rc=${rc}"; else bad "recheck $1: want rc=$4, got ${rc}${note}: $(tail -3 "${d}/log")"; fi
}
recheck_case matches          yes no 0
recheck_case differs          no  no 1
recheck_case already-landed   yes yes 1
# Landed by rebase with adds/modifies only: the patch applies as a NO-OP, so the
# re-check is the original red again — the checker's verdict, not an apply error.
recheck_case landed-noop      yes yes 1 recheck.sh addonly
if grep -q 'do not apply' "${work}/rc-landed-noop/log"; then bad "recheck landed-noop: refused at the apply, not by the checker"
else ok "recheck landed-noop: the red came from the checker (the patch applied as a no-op)"; fi
recheck_case symlink          yes no 1 recheck.sh symlink
recheck_case diverged         yes no 0 recheck.sh diverged
recheck_case conflict         yes no 1 recheck.sh conflict
# Schemas main carries an edit runner main does NOT generate: that drift is not
# the partner's, so the red must stand (a whole-file overlay would revert it
# and absorb it).
recheck_case mainahead        yes no 1 recheck.sh mainahead
if grep -q '^::error::' "${work}/rc-mainahead/log"; then bad "recheck mainahead: refused by a guard, not by the checker"
else ok "recheck mainahead: the red came from the checker (the 3-way apply succeeded)"; fi
# Only generated bindings are applied: a list with none (the toml alone) is
# refused, not diffed as the whole tree; the toml as a rename's other side, a
# listed script (plainly or through a dot segment) and a dotfile are refused,
# never applied.
recheck_case tomlonly         yes no 1 recheck.sh tomlonly
recheck_case tomlrename       yes no 1 recheck.sh tomlrename
recheck_case outside          yes no 1 recheck.sh outside
recheck_case dotdot           yes no 1 recheck.sh dotdot
recheck_case dotfile          yes no 1 recheck.sh dotfile
logs_refusal() { # logs_refusal <case> <substring>: the rc 1 was THIS refusal
  if grep -q "^::error::.*$2" "${work}/rc-$1/log"; then ok "recheck $1: refused with '$2'"
  else bad "recheck $1: no '$2' refusal: $(tail -2 "${work}/rc-$1/log")"; fi
}
logs_refusal tomlonly   'lists no binding file'
logs_refusal tomlrename 'renames or copies codegen/pending-registration.toml'
logs_refusal outside    'outside the generated bindings'
logs_refusal dotdot     'a path with a dot segment'
logs_refusal dotfile    'a path with a dot segment or dotfile'
# Each guard must be able to fail: without it, a case above flips.
sed '/reset -q --hard HEAD/d' "${work}/recheck.sh" >"${work}/recheck-noreset.sh"
sed '/clean -fdxq/d' "${work}/recheck.sh" >"${work}/recheck-noclean.sh"
sed '/100644|100755) : ;;/s/100644|100755/*/' "${work}/recheck.sh" >"${work}/recheck-nomode.sh"
for m in noreset noclean nomode; do
  cmp -s "${work}/recheck.sh" "${work}/recheck-${m}.sh" && bad "recheck mutation ${m} changed nothing"
done
recheck_case mut-noreset      yes no 1 recheck-noreset.sh
recheck_case mut-noclean      yes no 1 recheck-noclean.sh
recheck_case mut-nomode       yes no 0 recheck-nomode.sh symlink
# Without the path guards a partner script reaches the tree the step runs.
sed '/lists no binding file/d' "${work}/recheck.sh" >"${work}/recheck-noempty.sh"
sed '/outside the generated bindings/s/\*) echo .*$/*) : ;;/' "${work}/recheck.sh" >"${work}/recheck-nooutside.sh"
# git DOES resolve '..' in a pathspec, so without the dot-segment guard
# ts/src/generated/../../../scripts/note is scripts/note, applied and run.
sed '/a path with a dot segment or dotfile/d' "${work}/recheck.sh" >"${work}/recheck-nodot.sh"
for m in noempty nooutside nodot; do
  cmp -s "${work}/recheck.sh" "${work}/recheck-${m}.sh" && bad "recheck mutation ${m} changed nothing"
done
mut_leaks() { # mut_leaks <mutant> <extra>: the mutant must let the partner's script onto main
  local out; out="$(recheck_case "mut-$1" yes no 1 "recheck-$1.sh" "$2")"
  case "${out}" in *"partner script leaked"*) ok "recheck mutation $1 leaks the partner script (the guard is load-bearing)" ;;
    *) bad "recheck mutation $1 did not leak: ${out}" ;; esac
}
mut_leaks noempty   tomlonly
mut_leaks nooutside outside
mut_leaks nodot     dotdot

# The audit parses the re-check's OWN warning: take it from the matches case,
# so editing that wording without the audit's parser turns a case red.
absorb_msg="$(sed -n 's/^::warning title=runner main is ahead of schemas main (declared wait):://p' "${work}/rc-matches/log")"
[ -n "${absorb_msg}" ] && ok "recheck matches emitted the absorption warning" || bad "recheck matches emitted no absorption warning"
printf '%s' "${absorb_msg//$(cat "${work}/rc-matches/psha")/@@SHA@@}" >"${work}/absorb-msg"
grep -q '@@SHA@@' "${work}/absorb-msg" || bad "the absorption warning does not carry the full partner sha"

# The daily audit of an absorbed wait (qontinui-types-drift-wait-audit.yml):
# API answers are files, schemas is a local bare repo.
ad="${work}/audit"; mkdir -p "${ad}"
aup="${ad}/schemas.git"; git init -q --bare "${aup}"; git -C "${aup}" config uploadpack.allowAnySHA1InWant true
git init -q "${ad}/seed"; ag=(git -C "${ad}/seed" -c user.name=t -c user.email=t@t)
mkdir -p "${ad}/seed/ts/src/generated" "${ad}/seed/codegen"
printf 'v1\n' >"${ad}/seed/ts/src/generated/A.d.ts"; printf 'x\n' >"${ad}/seed/ts/src/generated/Gone.d.ts"; printf 'rows=0\n' >"${ad}/seed/codegen/pending-registration.toml"
printf '// barrel\nexport A\nexport B\nexport C\nexport D\n' >"${ad}/seed/ts/src/generated/index.ts"
"${ag[@]}" add -A; "${ag[@]}" commit -qm base; abase="$("${ag[@]}" rev-parse HEAD)"
printf 'v2\n' >"${ad}/seed/ts/src/generated/A.d.ts"; "${ag[@]}" rm -q ts/src/generated/Gone.d.ts; printf 'rows=1\n' >"${ad}/seed/codegen/pending-registration.toml"
printf 'export P\n' >>"${ad}/seed/ts/src/generated/index.ts"
"${ag[@]}" commit -qam partner; ahead="$("${ag[@]}" rev-parse HEAD)"
# "main after a merge/fast-forward land, then some other PR edited the toml": the
# compare reads behind (landedrebase below is the case decided by content).
printf 'rows=7\n' >"${ad}/seed/codegen/pending-registration.toml"; "${ag[@]}" commit -qam other; alanded="$("${ag[@]}" rev-parse HEAD)"
# "Landed by rebase": main moved on (another binding, which also edited the
# shared barrel), then the partner's changes were re-applied as a new commit —
# diverged, decided by content, and main's barrel is NOT the partner's blob.
"${ag[@]}" checkout -q -b rebased "${abase}"
printf 'z\n' >"${ad}/seed/ts/src/generated/Z.d.ts"; sed -i 's|^// barrel$|// barrel v2|' "${ad}/seed/ts/src/generated/index.ts"; "${ag[@]}" add -A; "${ag[@]}" commit -qm z
printf 'v2\n' >"${ad}/seed/ts/src/generated/A.d.ts"; "${ag[@]}" rm -q ts/src/generated/Gone.d.ts; printf 'export P\n' >>"${ad}/seed/ts/src/generated/index.ts"; "${ag[@]}" commit -qam partner-rebased; arebased="$("${ag[@]}" rev-parse HEAD)"
# "Diverged, NOT landed": main moved on, the partner's content is absent.
"${ag[@]}" checkout -q -b moved-on "${abase}"
printf 'y\n' >"${ad}/seed/ts/src/generated/Y.d.ts"; "${ag[@]}" add -A; "${ag[@]}" commit -qm y; amovedon="$("${ag[@]}" rev-parse HEAD)"
git -C "${ad}/seed" push -q "${aup}" "${ahead}:refs/heads/partner" "${alanded}:refs/heads/landed" "${arebased}:refs/heads/rebased" "${amovedon}:refs/heads/moved-on"
python3 - "${ad}" "${ahead}" "${work}/absorb-msg" "${abase}" <<'PY'
import json, os, sys
ad, head, absorb, mbase = sys.argv[1], sys.argv[2], open(sys.argv[3]).read(), sys.argv[4]
R = 'repos_qontinui_qontinui-runner_actions_'
def w(case, name, body):
    d = os.path.join(ad, 'fx', case); os.makedirs(d, exist_ok=True)
    open(os.path.join(d, name), 'w').write(json.dumps(body))
def msg(sha=head):
    return absorb.replace('@@SHA@@', sha)
TITLE = 'runner main is ahead of schemas main (declared wait)'
runs = R + 'workflows_qontinui-types-drift.yml_runs_event_push_branch_main_status_completed_per_page_20'
# files=None: run.sh writes the compare from the local repo against whichever
# main the case runs on, the way GitHub would answer it.
# A newer run whose drift check passed (the partner search skipped): coord read it.
CLEAN_RUN = [{'name': 'Check for drift', 'conclusion': 'success'}, {'name': 'Find a waiting runner-leads partner', 'conclusion': 'skipped'},
             {'name': 'Re-check against the waiting partner', 'conclusion': 'skipped'}, {'name': 'Decide the drift verdict', 'conclusion': 'skipped'}]
def newer_jobs(case, rid, steps=CLEAN_RUN):
    w(case, R + 'runs_%d_jobs_per_page_100' % rid, {'jobs': [{'name': 'check-drift', 'id': 50 + rid, 'steps': steps}]})
def base(case, concl='success', absorbed=True, ann=True, m=None, jobname='check-drift', drift_step=True, files=None, cmp=True, newer=(), newer_steps=CLEAN_RUN):
    w(case, runs, {'workflow_runs': [{'id': 100 + i, 'conclusion': c, 'head_sha': 'e' * 40} for i, c in enumerate(newer)]
                                    + [{'id': 7, 'conclusion': concl, 'head_sha': 'c' * 40}]})
    for i in range(len(newer)):
        newer_jobs(case, 100 + i, newer_steps)
    steps = [{'name': 'Check for drift', 'conclusion': 'success'}] if drift_step else [{'name': 'Something else', 'conclusion': 'success'}]
    steps.append({'name': 'Re-check against the waiting partner', 'conclusion': 'success' if absorbed else 'skipped'})
    w(case, R + 'runs_7_jobs_per_page_100', {'jobs': [{'name': jobname, 'id': 9, 'steps': steps}]})
    w(case, 'repos_qontinui_qontinui-runner_check-runs_9_annotations_per_page_100',
      [{'annotation_level': 'warning', 'title': TITLE, 'message': m or msg()}] if ann else [{'annotation_level': 'warning', 'title': 'other', 'message': 'x'}])
    if cmp and files is not None:
        w(case, 'repos_qontinui_qontinui-schemas_compare_main...' + head,
          {'status': 'diverged', 'merge_base_commit': {'sha': mbase}, 'files': files} if files != 'none'
          else {'status': 'diverged', 'merge_base_commit': {'sha': mbase}, 'message': 'x'})
def pr(case, state='open', sha=head, base_ref='main', merged=None):
    w(case, 'repos_qontinui_qontinui-schemas_pulls_285', {'state': state, 'head': {'sha': sha}, 'base': {'ref': base_ref}, 'merged_at': merged, 'labels': []})
w('noruns', runs, {'workflow_runs': []})
w('noarray', runs, {})
base('badstrip', m=msg().replace('coord stripped the declaration 2026-11-05T08:15:55Z)', 'coord stripped the declaration 2026-13-45T99:99:99Z)')); pr('badstrip')
base('red', concl='failure')
base('plain', absorbed=False)
base('renamedjob', jobname='Check drift'); pr('renamedjob')
base('nodriftstep', drift_step=False)
base('noann', ann=False)
base('landedcontent'); pr('landedcontent', state='open')
base('mergedelsewhere'); pr('mergedelsewhere', state='closed', merged='2026-11-05T12:00:00Z')
base('retargeted'); pr('retargeted', base_ref='release')
base('waiting'); pr('waiting')
base('expired'); pr('expired')
base('closed'); pr('closed', state='closed')
base('moved'); pr('moved', sha='d' * 40)
base('cmpnofiles', files='none'); pr('cmpnofiles')
base('cmp300', files=[{'filename': 'ts/src/generated/A.d.ts', 'status': 'modified'}] * 300); pr('cmp300')
base('expiredmod', files=[{'filename': 'ts/src/generated/A.d.ts', 'status': 'modified'}]); pr('expiredmod')
base('expiredrm', files=[{'filename': 'ts/src/generated/Gone.d.ts', 'status': 'removed'}]); pr('expiredrm')
base('badmsg', m='absorbed, but names nothing'); pr('badmsg')
# The absorbed green is audited even behind a newer cancelled or red run.
base('cancelledafter', newer=('cancelled',)); pr('cancelledafter')
base('redafter', newer=('failure',)); pr('redafter')
w('alltimedout', runs, {'workflow_runs': [{'id': 1, 'conclusion': 'timed_out', 'head_sha': 'e' * 40}]})
newer_jobs('allcancelled', 1); newer_jobs('allcancelled', 2)
w('allcancelled', runs, {'workflow_runs': [{'id': 1, 'conclusion': 'cancelled', 'head_sha': 'e' * 40},
                                           {'id': 2, 'conclusion': 'stale', 'head_sha': 'e' * 40}]})
base('landedff'); pr('landedff')
base('landedrebase'); pr('landedrebase')
base('divergednotlanded'); pr('divergednotlanded')
base('auditnomb', files=[{'filename': 'ts/src/generated/A.d.ts', 'status': 'modified'}]); pr('auditnomb')
base('nostatus', files=[{'filename': 'ts/src/generated/A.d.ts', 'status': 'modified'}]); pr('nostatus')
base('phantom', files=[{'filename': 'ts/src/generated/A.d.ts', 'status': 'modified'}, {'filename': 'ts/src/generated/Phantom.d.ts', 'status': 'added'}]); pr('phantom')
# The re-check step renamed away: "absorbed nothing" would be a guess.
base('norecheckstep'); pr('norecheckstep')
w('norecheckstep', R + 'runs_7_jobs_per_page_100', {'jobs': [{'name': 'check-drift', 'id': 9, 'steps': [{'name': 'Check for drift', 'conclusion': 'success'}]}]})
# Two different absorption messages (two partners, or a forged one).
base('twoann'); pr('twoann')
w('twoann', 'repos_qontinui_qontinui-runner_check-runs_9_annotations_per_page_100',
  [{'annotation_level': 'warning', 'title': TITLE, 'message': msg()}, {'annotation_level': 'warning', 'title': TITLE, 'message': msg('f' * 40)}])
# An unreadable entry must not drop the "present" half: here only the removal would be checked.
base('badentry', files=[{'status': 'added'}, {'filename': 'ts/src/generated/Gone.d.ts', 'status': 'removed'}]); pr('badentry')
# A drift red that a cancel or a lost runner left with no decide verdict, newer
# than the green: coord may hold the older green over it. A real red (decide
# failed) newer than the green is coord's to see, and the audit goes on.
RED_NO_VERDICT_CANCEL = [{'name': 'Check for drift', 'conclusion': 'failure'}, {'name': 'Find a waiting runner-leads partner', 'conclusion': 'success'},
                         {'name': 'Re-check against the waiting partner', 'conclusion': 'cancelled'}, {'name': 'Decide the drift verdict', 'conclusion': 'skipped'}]
RED_RUNNER_LOST = [{'name': 'Check for drift', 'conclusion': 'success'}, {'name': 'Find a waiting runner-leads partner', 'conclusion': 'success'},
                   {'name': 'Re-check against the waiting partner', 'conclusion': None}, {'name': 'Decide the drift verdict', 'conclusion': None}]
RED_REAL = [{'name': 'Check for drift', 'conclusion': 'success'}, {'name': 'Find a waiting runner-leads partner', 'conclusion': 'success'},
            {'name': 'Re-check against the waiting partner', 'conclusion': 'skipped'}, {'name': 'Decide the drift verdict', 'conclusion': 'failure'}]
base('cancelledred', newer=('cancelled',), newer_steps=RED_NO_VERDICT_CANCEL); pr('cancelledred')
base('runnerlost', newer=('failure',), newer_steps=RED_RUNNER_LOST); pr('runnerlost')
base('realred', newer=('failure',), newer_steps=RED_REAL); pr('realred')
# An error-level annotation with the same title is not an absorption message.
base('errorann'); pr('errorann')
w('errorann', 'repos_qontinui_qontinui-runner_check-runs_9_annotations_per_page_100',
  [{'annotation_level': 'warning', 'title': TITLE, 'message': msg()}, {'annotation_level': 'failure', 'title': TITLE, 'message': msg('f' * 40)}])
PY
# nostatus / auditnomb: compares with no status, and with no merge base.
python3 - "${ad}/fx/nostatus/repos_qontinui_qontinui-schemas_compare_main...${ahead}" "${ad}/fx/auditnomb/repos_qontinui_qontinui-schemas_compare_main...${ahead}" <<'PY'
import json, sys
for p, k in zip(sys.argv[1:], ('status', 'merge_base_commit')):
    d = json.load(open(p)); d.pop(k); json.dump(d, open(p, 'w'))
PY
write_compare() { # write_compare <fixture dir> <main sha>: GitHub's main...<absorbed> compare, from git
  local f="$1/repos_qontinui_qontinui-schemas_compare_main...${ahead}" mb st
  [ ! -f "${f}" ] || return 0
  mb="$(git -C "${ad}/seed" merge-base "$2" "${ahead}")"
  if [ "$2" = "${ahead}" ]; then st=identical; elif [ "${mb}" = "${ahead}" ]; then st=behind
  elif [ "${mb}" = "$2" ]; then st=ahead; else st=diverged; fi
  git -C "${ad}/seed" diff --name-status -M "${mb}" "${ahead}" | python3 -c '
import json, sys
S = {"A": "added", "M": "modified", "D": "removed", "R": "renamed"}
files = []
for line in sys.stdin:
    p = line.rstrip("\n").split("\t")
    e = {"status": S[p[0][0]], "filename": p[-1]}
    if p[0][0] == "R": e["previous_filename"] = p[1]
    files.append(e)
json.dump({"status": sys.argv[1], "merge_base_commit": {"sha": sys.argv[2]}, "files": files}, sys.stdout)' "${st}" "${mb}" >"${f}"
}
audit_case() { # audit_case <fixture> <schemas main: base|landed|ff|rebased|moved-on> <now-iso> <want-rc> [<want ::error title substring>] [<log substring>]
  local mref
  case "$2" in base) mref="${abase}" ;; landed) mref="${alanded}" ;; ff) mref="${ahead}" ;; rebased) mref="${arebased}" ;; moved-on) mref="${amovedon}" ;; esac
  git -C "${ad}/seed" push -q -f "${aup}" "${mref}:refs/heads/main"
  write_compare "${ad}/fx/$1" "${mref}"
  set +e
  (cd "${ad}" && DRIFT_WAIT_TEST_API="${ad}/fx/$1" DRIFT_WAIT_TEST_GIT_URL="${aup}" DRIFT_WAIT_TEST_NOW="$(date -u -d "$3" +%s)" \
     GH_TOKEN=x RUNNER_REPO=qontinui/qontinui-runner bash "${work}/audit.sh" >"${ad}/log.$1" 2>&1)
  local rc=$?
  set -e
  if [ "${rc}" = "$4" ] && { [ -z "${5:-}" ] || grep -qF "::error title=drift wait audit: ${5}::" "${ad}/log.$1"; } \
     && { [ -z "${6:-}" ] || grep -qF "$6" "${ad}/log.$1"; }; then ok "audit $1 (main=$2) -> rc=${rc} ${5:-} ${6:+[$6]}"
  else bad "audit $1 (main=$2): want rc=$4 '${5:-}' '${6:-}', got ${rc}: $(tail -2 "${ad}/log.$1")"; fi
}
audit_case noruns          base   2026-11-05T12:00:00Z 0 "" "nothing to audit"
audit_case noarray         base   2026-11-05T12:00:00Z 1 "UNKNOWN" "no workflow_runs array"
audit_case badstrip        base   2026-11-05T12:00:00Z 1 "UNKNOWN" "is not a date"
audit_case red             base   2026-11-05T12:00:00Z 0 "" "is green"
audit_case plain           base   2026-11-05T12:00:00Z 0 "" "absorbed nothing"
audit_case renamedjob      base   2026-11-05T12:00:00Z 0 "" "Waiting, valid"
audit_case nodriftstep     base   2026-11-05T12:00:00Z 1 "UNKNOWN" "ran a 'Check for drift' step"
audit_case noann           base   2026-11-05T12:00:00Z 1 "UNKNOWN" "carries no"
audit_case landedcontent   landed 2026-11-09T12:00:00Z 0
audit_case mergedelsewhere base   2026-11-05T18:00:00Z 1 "partner gone unlanded"
audit_case retargeted      base   2026-11-05T18:00:00Z 1 "partner gone unlanded"
audit_case waiting         base   2026-11-05T18:00:00Z 0
audit_case expired         base   2026-11-08T12:00:00Z 1 "wait expired"
audit_case closed          base   2026-11-05T18:00:00Z 1 "partner gone unlanded"
audit_case moved           base   2026-11-05T18:00:00Z 1 "partner moved"
audit_case cmpnofiles      base   2026-11-05T18:00:00Z 1 "UNKNOWN" "no files array"
# cmp300 runs on a main that carries the content: without the ceiling guard the
# 300 listed (real) files would reverse-apply and read as a false ENDED.
audit_case cmp300          rebased 2026-11-05T18:00:00Z 1 "UNKNOWN" "the API ceiling"
audit_case expiredmod      base   2026-11-08T12:00:00Z 1 "wait expired"
audit_case expiredrm       base   2026-11-08T12:00:00Z 1 "wait expired"
audit_case badmsg          base   2026-11-05T12:00:00Z 1 "UNKNOWN" "does not name its partner"
audit_case cancelledafter  base   2026-11-08T12:00:00Z 1 "wait expired"
audit_case redafter        base   2026-11-08T12:00:00Z 1 "wait expired"
audit_case allcancelled    base   2026-11-05T12:00:00Z 1 "UNKNOWN"
audit_case alltimedout     base   2026-11-05T12:00:00Z 0 "" "is green"
audit_case landedff        ff     2026-11-09T12:00:00Z 0
audit_case landedrebase    rebased 2026-11-09T12:00:00Z 0
audit_case divergednotlanded moved-on 2026-11-08T12:00:00Z 1 "wait expired"
audit_case auditnomb       base   2026-11-05T18:00:00Z 1 "UNKNOWN" "has no merge base"
audit_case nostatus        base   2026-11-05T18:00:00Z 1 "UNKNOWN" "has no status"
audit_case phantom         base   2026-11-05T18:00:00Z 1 "UNKNOWN" "has no blob at"
audit_case norecheckstep   base   2026-11-05T18:00:00Z 1 "UNKNOWN" "lists a 'Re-check against the waiting partner' step"
audit_case twoann          base   2026-11-05T18:00:00Z 1 "UNKNOWN" "different"
audit_case badentry        rebased 2026-11-05T18:00:00Z 1 "UNKNOWN" "a file entry the audit cannot read"
audit_case cancelledred    base   2026-11-05T18:00:00Z 1 "drift red read as no verdict"
audit_case runnerlost      base   2026-11-05T18:00:00Z 1 "drift red read as no verdict"
audit_case realred         base   2026-11-05T18:00:00Z 0 "" "Waiting, valid"
audit_case errorann        base   2026-11-05T18:00:00Z 0 "" "Waiting, valid"
grep -q "Waiting, valid" "${ad}/log.waiting" && ok "audit waiting says why it is green" || bad "audit waiting: no 'Waiting, valid' line"
for c in landedcontent landedff landedrebase; do
  grep -q "The wait has ENDED" "${ad}/log.${c}" && ok "audit ${c} says the wait ended" || bad "audit ${c}: no 'ENDED' line"
done
grep -q "by content (compare diverged)" "${ad}/log.landedrebase" && ok "audit landedrebase decided it by content" || bad "audit landedrebase: not decided by content"

# Structure: the properties that keep the red from failing open.
python3 - "${wf}" <<'PY' || fails=$((fails + 1))
import sys, yaml
wfd = yaml.safe_load(open(sys.argv[1]))
auditd = yaml.safe_load(open(sys.argv[1].replace('qontinui-types-drift.yml', 'qontinui-types-drift-wait-audit.yml')))
steps = wfd['jobs']['check-drift']['steps']
audit_run = auditd['jobs']['audit']['steps'][0]['run']
by = {s.get('name'): s for s in steps}
names = [s.get('name') for s in steps]
bad = []
# Triggers (PyYAML reads the `on:` key as True). The audit must never set a main
# baseline of its own, and a scheduled drift run would re-ask on a timer.
if set((auditd.get(True) or auditd.get('on') or {}).keys()) != {'schedule', 'workflow_dispatch'}:
    bad.append('the audit must trigger only on schedule and workflow_dispatch: %r' % sorted((auditd.get(True) or {}).keys()))
if 'schedule' in (wfd.get(True) or wfd.get('on') or {}):
    bad.append('the drift workflow must not be scheduled')
drift, decide = by['Check for drift'], by['Decide the drift verdict']
recheck = by['Re-check against the waiting partner']
if drift.get('id') != 'drift':
    bad.append('drift step id must be drift')
if str(drift.get('continue-on-error')) != "${{ github.event_name != 'pull_request' }}":
    bad.append('drift continue-on-error must be exactly the non-pull_request expression: %r' % drift.get('continue-on-error'))
want_if = "${{ !cancelled() && github.event_name != 'pull_request' && steps.drift.outcome == 'failure' }}"
if str(decide.get('if')) != want_if:
    bad.append('decide must run on every non-PR drift failure, independent of the partner/re-check outcomes: %r' % decide.get('if'))
if 'continue-on-error' in decide:
    bad.append('decide is the gate; it must not be continue-on-error')
if names.index('Decide the drift verdict') != len(names) - 1:
    bad.append('decide must be the last step')
if names.index('Check for drift') > names.index('Find a waiting runner-leads partner') or names.index('Find a waiting runner-leads partner') > names.index('Re-check against the waiting partner'):
    bad.append('step order must be drift, partner, re-check, decide')
partner = by['Find a waiting runner-leads partner']
if partner.get('id') != 'partner':
    bad.append('partner step id must be partner')
if str(partner.get('if')) != "${{ github.event_name != 'pull_request' && steps.drift.outcome == 'failure' }}":
    bad.append('partner step must run exactly on a non-PR drift failure: %r' % partner.get('if'))
if recheck.get('id') != 'recheck':
    bad.append('re-check step id must be recheck')
if str(recheck.get('if')) != "${{ !cancelled() && steps.partner.outcome == 'success' && steps.partner.outputs.found == 'true' }}":
    bad.append('re-check must run only on a successful search that found a partner: %r' % recheck.get('if'))
for s in (partner, recheck):
    if not isinstance(s.get('timeout-minutes'), int):
        bad.append('%s needs a step timeout-minutes: a hang must fail the step, not cancel the run' % s.get('name'))
    if 'continue-on-error' in s:
        bad.append('%s must not be continue-on-error: its failure must leave the red standing' % s.get('name'))
if (decide.get('env') or {}).get('RECHECK') != '${{ steps.recheck.outcome }}':
    bad.append('decide must read the RE-CHECK outcome, nothing else: %r' % (decide.get('env') or {}).get('RECHECK'))
if 'scripts/check-generated-drift.sh --verify' not in recheck['run'] or 'generate_types.sh' not in recheck['run']:
    bad.append('the re-check must regenerate and run the same shared checker')
# The audit keys on these step names and on the warning's title: a rename on
# one side only must fail here, not turn the audit silently green.
for n in ('Check for drift', 'Re-check against the waiting partner'):
    if n not in names or ('"%s"' % n) not in audit_run:
        bad.append('step name %r must exist in the workflow and be the literal the audit keys on' % n)
title = 'runner main is ahead of schemas main (declared wait)'
if ('::warning title=%s::' % title) not in recheck['run'] or ("TITLE='%s'" % title) not in audit_run:
    bad.append('the absorption warning title must match between the re-check and the audit')
if bad:
    print('FAIL structure:', bad); sys.exit(1)
print('ok   structure: drift red decided only by the last step, absorbed only by a green full re-check')
PY

if [ "${fails}" -ne 0 ]; then echo "${fails} failure(s)"; exit 1; fi
echo "all passed"
