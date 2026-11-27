#!/usr/bin/env bash
#
# Tests for `prepush-stash-guard.sh` and for cargo-prepush.sh's staging rule —
# the two halves of the fix for coord findings 690aaeec / d0958551 (a pre-push
# run reverted a worktree's unstaged edits for 13 minutes while cargo-guard
# builds in it compiled the index instead).
#
# Run: bash .pre-commit-hooks/prepush-stash-guard-test.sh
#
# Throwaway git repos under $TMPDIR. Three groups:
#   1. the guard against hand-made stash patches (no pre-commit needed);
#   2. the guard under the REAL pre-commit framework, end to end: a real
#      `pre-commit run --hook-stage pre-push --from-ref --to-ref` stashes, the
#      guard refuses, `fail_fast` skips the later hook, and the edit is back.
#      SKIPPED WITH A STATED REASON when pre-commit is not importable;
#   3. cargo-prepush.sh run with NO stash (the shim path) must not stage the
#      caller's unstaged edits. `cargo` is a recording shim on PATH.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GUARD="$SCRIPT_DIR/prepush-stash-guard.sh"
PREPUSH="$SCRIPT_DIR/cargo-prepush.sh"

# Git exports GIT_DIR and friends to every hook it runs; they override `git -C`
# and would aim the fixtures at the real repository. Same list as
# push-range-test.sh.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
    GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR GIT_PREFIX \
    GIT_INTERNAL_SUPER_PREFIX GIT_CONFIG GIT_CONFIG_COUNT \
    GIT_CONFIG_GLOBAL GIT_CONFIG_SYSTEM GIT_NAMESPACE \
    GIT_INDEX_VERSION GIT_QUARANTINE_PATH GIT_PUSH_CERT \
    GIT_REFLOG_ACTION
unset PRE_COMMIT PRE_COMMIT_HOME PRE_COMMIT_FROM_REF PRE_COMMIT_TO_REF \
    QONTINUI_PREPUSH_ALLOW_STASH QONTINUI_PREPUSH_STASH_WINDOW_SECS \
    QONTINUI_PREPUSH_STASH_STORE QONTINUI_PREPUSH_SKIP QONTINUI_PREPUSH_SKIP_ALL \
    QONTINUI_PREPUSH_STRICT CARGO_TARGET_DIR QONTINUI_PREPUSH_CARGO_GUARD QONTINUI_ROOT \
    QONTINUI_PREPUSH_TREE_GUARD_LIB_DIR SKIP
export QONTINUI_PREPUSH_NO_TTY=1

PASS=0
FAIL=0
SKIPPED=0
ROOTS=()
cleanup() { local r; for r in ${ROOTS[@]+"${ROOTS[@]}"}; do rm -rf "$r"; done; }
trap cleanup EXIT

ok()   { PASS=$((PASS + 1)); printf '  ok   %s\n' "$1"; }
bad()  { FAIL=$((FAIL + 1)); printf '  FAIL %s\n' "$1"; [ -n "${2:-}" ] && printf '%s\n' "$2" | sed 's/^/         /'; }
skip() { SKIPPED=$((SKIPPED + 1)); printf '  skip %s — %s\n' "$1" "$2"; }
check() { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1" "want: $2"$'\n'"got:  $3"$'\n'"${4:-}"; fi; }

# A repo with one commit holding src-tauri/a.rs and src-tauri/b.rs.
new_repo() {
    local root
    root="$(mktemp -d)"
    ROOTS+=("$root")
    WORK="$root/repo"
    STORE="$root/store"
    mkdir -p "$WORK/src-tauri" "$STORE"
    git -C "$WORK" init --quiet
    git -C "$WORK" config user.email t@example.invalid
    git -C "$WORK" config user.name test
    git -C "$WORK" config core.autocrlf false
    printf 'fn a() {}\n' > "$WORK/src-tauri/a.rs"
    printf 'fn b() {}\n' > "$WORK/src-tauri/b.rs"
    git -C "$WORK" add -A
    git -C "$WORK" commit --quiet -m base
    BASE_SHA="$(git -C "$WORK" rev-parse HEAD)"
}

# Reproduce exactly what pre-commit's staged_files_only does: write the
# unstaged diff to <store>/patch<epoch>-<pid>, then `git checkout -- .`.
# The pid in the name must be THIS run's pre-commit process. On Git Bash /
# MSYS the guard only requires it to be a live Windows pid, so this shell's
# Windows pid stands in. Elsewhere the guard requires pid == its $PPID, so
# run_guard launches it from a wrapper shell that first renames a LIVE_PID
# patch to its own pid (and back afterwards).
LIVE_PID="$$"
[ -r "/proc/$$/winpid" ] && LIVE_PID="$(cat "/proc/$$/winpid")"
DEAD_PID=999999937
fake_stash() {
    local epoch="$1" pid="${2:-$LIVE_PID}"
    PATCH="$STORE/patch${epoch}-${pid}"
    (cd "$WORK" && git diff-index --ignore-submodules --binary --no-color --no-ext-diff \
        "$(git write-tree)" -- > "$PATCH")
    git -C "$WORK" -c submodule.recurse=0 checkout -- .
}

run_guard() {
    GUARD_OUT="$(cd "$WORK" && QONTINUI_PREPUSH_STASH_STORE="$STORE" "$@" bash -c '
        live="$1"; store="$2"; guard="$3"
        for f in "$store"/patch*-"$live"; do
            [ -f "$f" ] && [ ! -r "/proc/$$/winpid" ] && mv "$f" "${f%-*}-$$"
        done
        bash "$guard"; rc=$?
        for f in "$store"/patch*-"$$"; do
            [ -f "$f" ] && [ ! -r "/proc/$$/winpid" ] && mv "$f" "${f%-*}-$live"
        done
        exit "$rc"' _ "$LIVE_PID" "$STORE" "$GUARD" 2>&1)"
    GUARD_RC=$?
}

echo "prepush-stash-guard: hand-made stash patches"

new_repo
printf 'fn a() { /* in progress */ }\n' > "$WORK/src-tauri/a.rs"
fake_stash "$(date +%s)"
run_guard env -u PRE_COMMIT
check "outside the framework (no PRE_COMMIT) -> exit 0, nothing to guard" 0 "$GUARD_RC" "$GUARD_OUT"
run_guard env PRE_COMMIT=1
check "fresh patch that applies to this tree -> refused (exit 1)" 1 "$GUARD_RC" "$GUARD_OUT"
case "$GUARD_OUT" in
    *REFUSED*"$STORE/patch"*) ok "and the refusal names the patch" ;;
    *) bad "and the refusal names the patch" "$GUARD_OUT" ;;
esac
check "the guard itself leaves the tree untouched (still reverted, framework restores)" \
    "fn a() {}" "$(cat "$WORK/src-tauri/a.rs")"
run_guard env PRE_COMMIT=1 QONTINUI_PREPUSH_ALLOW_STASH=1
check "QONTINUI_PREPUSH_ALLOW_STASH=1 -> proceeds (exit 0)" 0 "$GUARD_RC" "$GUARD_OUT"
case "$GUARD_OUT" in
    *WARNING*) ok "and warns that builds compile the index" ;;
    *) bad "and warns that builds compile the index" "$GUARD_OUT" ;;
esac
# The framework restores: the same patch now does NOT apply (edits present).
git -C "$WORK" apply "$PATCH"
run_guard env PRE_COMMIT=1
check "patch already restored (edits present) -> exit 0" 0 "$GUARD_RC" "$GUARD_OUT"

new_repo
printf 'fn b() { 1 }\n' > "$WORK/src-tauri/b.rs"
fake_stash "$(( $(date +%s) - 3600 ))"
run_guard env PRE_COMMIT=1
check "patch older than the window -> not this run's, exit 0" 0 "$GUARD_RC" "$GUARD_OUT"
run_guard env PRE_COMMIT=1 QONTINUI_PREPUSH_STASH_WINDOW_SECS=7200
check "same patch inside a widened window -> refused" 1 "$GUARD_RC" "$GUARD_OUT"

new_repo
printf 'fn b() { 2 }\n' > "$WORK/src-tauri/b.rs"
fake_stash "$(date +%s)" "$DEAD_PID"
run_guard env PRE_COMMIT=1
check "fresh applicable patch from a process that has EXITED -> stale, exit 0" 0 "$GUARD_RC" "$GUARD_OUT"

new_repo
printf 'fn b() { 3 }\n' > "$WORK/src-tauri/b.rs"
if [ -r "/proc/$$/winpid" ]; then
    skip "a live patch whose writer is NOT this hook's parent -> exit 0" \
        "Git Bash/MSYS cannot read a native parent's pid; there the guard only checks liveness (stated residual)"
else
    # pid 1 is alive but is never the hook's parent.
    fake_stash "$(date +%s)" 1
    run_guard env PRE_COMMIT=1
    check "a live patch whose writer is NOT this hook's parent (another worktree's run) -> exit 0" \
        0 "$GUARD_RC" "$GUARD_OUT"
fi

new_repo
run_guard env PRE_COMMIT=1
check "clean tree, empty store -> exit 0" 0 "$GUARD_RC" "$GUARD_OUT"
GUARD_OUT="$(cd "$WORK" && PRE_COMMIT=1 QONTINUI_PREPUSH_STASH_STORE="$STORE/absent" bash "$GUARD" 2>&1)"
check "unreadable/absent store -> exit 0 (never worse than before)" 0 "$?" "$GUARD_OUT"

echo
echo "prepush-stash-guard: under the real pre-commit framework"

PY=""
for cand in python3 python; do
    if command -v "$cand" >/dev/null 2>&1 && "$cand" -c 'import pre_commit' >/dev/null 2>&1; then
        PY="$cand"; break
    fi
done
if [ -z "$PY" ]; then
    skip "real pre-commit pre-push run" "pre_commit is not importable on this machine"
else
    new_repo
    mkdir -p "$WORK/.pre-commit-hooks"
    cp "$GUARD" "$WORK/.pre-commit-hooks/prepush-stash-guard.sh"
    cat > "$WORK/.pre-commit-hooks/record.sh" <<'REC'
#!/usr/bin/env bash
# Records that it ran and what it saw in a.rs.
printf 'ran saw=%s\n' "$(cat src-tauri/a.rs)" >> "$RECORD_LOG"
REC
    cat > "$WORK/.pre-commit-config.yaml" <<'CFG'
repos:
  - repo: local
    hooks:
      - id: prepush-stash-guard
        name: guard
        entry: bash .pre-commit-hooks/prepush-stash-guard.sh
        language: system
        stages: [pre-push]
        pass_filenames: false
        always_run: true
        fail_fast: true
      - id: record
        name: record
        entry: bash .pre-commit-hooks/record.sh
        language: system
        stages: [pre-push]
        pass_filenames: false
        always_run: true
CFG
    git -C "$WORK" add -A
    git -C "$WORK" commit --quiet -m hooks
    TO_SHA="$(git -C "$WORK" rev-parse HEAD)"
    RECORD_LOG="$(dirname "$WORK")/record.log"
    : > "$RECORD_LOG"
    EDIT='fn a() { /* caller edit */ }'
    printf '%s\n' "$EDIT" > "$WORK/src-tauri/a.rs"

    FW_OUT="$(cd "$WORK" && RECORD_LOG="$RECORD_LOG" PRE_COMMIT_HOME="$STORE" \
        "$PY" -m pre_commit run --hook-stage pre-push \
        --from-ref "$BASE_SHA" --to-ref "$TO_SHA" 2>&1)"
    FW_RC=$?
    if printf '%s' "$FW_OUT" | grep -q 'Stashing unstaged files'; then
        ok "the framework stashed (precondition: this is the defect's shape)"
    else
        bad "the framework stashed (precondition: this is the defect's shape)" "$FW_OUT"
    fi
    [ "$FW_RC" -ne 0 ] && ok "pre-push run fails (rc=$FW_RC)" || bad "pre-push run fails" "$FW_OUT"
    check "fail_fast: the later hook never ran over the reverted tree" "" "$(cat "$RECORD_LOG")" "$FW_OUT"
    check "the caller's edit is back in the working tree" "$EDIT" "$(cat "$WORK/src-tauri/a.rs")"

    # Clean tree: nothing is stashed, the guard passes, the later hook runs.
    git -C "$WORK" checkout --quiet -- .
    FW_OUT="$(cd "$WORK" && RECORD_LOG="$RECORD_LOG" PRE_COMMIT_HOME="$STORE" \
        "$PY" -m pre_commit run --hook-stage pre-push \
        --from-ref "$BASE_SHA" --to-ref "$TO_SHA" 2>&1)"
    check "clean tree -> pre-push run passes" 0 "$?" "$FW_OUT"
    check "and the later hook ran" "ran saw=fn a() {}" "$(cat "$RECORD_LOG")" "$FW_OUT"
fi

echo
echo "cargo-prepush.sh: no stash (shim path) must not stage the caller's edits"

new_repo
mkdir -p "$(dirname "$WORK")/bin"
SHIM_BIN="$(dirname "$WORK")/bin"
cat > "$SHIM_BIN/cargo" <<'SHIM'
#!/usr/bin/env bash
if [ "${1:-}" = metadata ]; then
    printf '{"packages":[],"target_directory":"%s","version":1}\n' "$PWD/target"
    exit 0
fi
if [ "${1:-}" = fmt ]; then
    # "Reformat" every .rs file under the cwd (src-tauri/), except one that
    # says it is already formatted.
    for f in *.rs; do grep -q nofmt "$f" || printf '// fmt\n' >> "$f"; done
fi
exit 0
SHIM
chmod +x "$SHIM_BIN/cargo"
printf 'fn c() {}\n' > "$WORK/src-tauri/c.rs"
git -C "$WORK" add src-tauri/c.rs
git -C "$WORK" commit --quiet -m c
printf 'fn a() { /* unstaged caller edit */ }\n' > "$WORK/src-tauri/a.rs"
printf 'fn c() { /* unstaged, nofmt */ }\n' > "$WORK/src-tauri/c.rs"
PP_OUT="$(cd "$WORK" && PATH="$SHIM_BIN:$PATH" QONTINUI_PREPUSH_SKIP=1 bash "$PREPUSH" 2>&1 </dev/null)"
PP_RC=$?
check "cargo-prepush exits 0" 0 "$PP_RC" "$PP_OUT"
check "a file that was clean before fmt is staged with fmt's rewrite" \
    "src-tauri/b.rs" "$(git -C "$WORK" diff --cached --name-only)" "$PP_OUT"
check "the caller's dirty file is NOT staged (index still holds HEAD)" \
    "fn a() {}" "$(git -C "$WORK" show :src-tauri/a.rs)"
check "and the caller's edit is still in the working tree" \
    "fn a() { /* unstaged caller edit */ }" "$(head -1 "$WORK/src-tauri/a.rs")"
case "$PP_OUT" in
    *"UNSTAGED edits of yours"*"src-tauri/a.rs"*) ok "and the NOTE names the file it left unstaged" ;;
    *) bad "and the NOTE names the file it left unstaged" "$PP_OUT" ;;
esac
case "$PP_OUT" in
    *"src-tauri/c.rs"*) bad "a dirty file fmt did NOT rewrite is not named" "$PP_OUT" ;;
    *) ok "a dirty file fmt did NOT rewrite is not named" ;;
esac
check "and that file is not staged either" "fn c() {}" "$(git -C "$WORK" show :src-tauri/c.rs)"

echo
printf 'passed: %d  failed: %d  skipped: %d\n' "$PASS" "$FAIL" "$SKIPPED"
[ "$FAIL" -eq 0 ]
