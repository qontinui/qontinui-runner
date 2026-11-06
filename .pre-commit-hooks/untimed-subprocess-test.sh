#!/usr/bin/env bash
#
# Tests for `untimed-subprocess.sh`, the bash entry the git-hooks shim can run.
# Asserts the wrapper's contract: repo-root resolution (env var, then git, never
# its own location), python3 -> python fallback, argument pass-through, exit
# code preservation, and that the no-argument run (the shim's shape) scans the
# real tree.
#
# Run: bash .pre-commit-hooks/untimed-subprocess-test.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WRAPPER="$HERE/untimed-subprocess.sh"
# Run as a pre-commit hook, git exports GIT_INDEX_FILE (and may export GIT_DIR
# and friends) pointing at THIS repo; left set, they hijack the git calls this
# test makes inside its scratch repos. The test's own location is a fine root
# for the TEST — only the wrapper is forbidden from deriving it that way.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_PREFIX GIT_COMMON_DIR \
    GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES QONTINUI_HOOK_REPO_ROOT
REPO_ROOT="$(cd "$HERE/.." && pwd)"

pass=0; failn=0
ok()  { echo "  ok   $*"; pass=$((pass + 1)); }
bad() { echo "  FAIL $*"; failn=$((failn + 1)); }

TMP="$(mktemp -d -t untimed-wrapper-test-XXXXXXXX)"
trap 'rm -rf "$TMP"' EXIT

# A fake repo whose "checker" records its argv and cwd, and exits with $STUB_RC.
mk_fake_repo() {
    local dir="$1"
    mkdir -p "$dir/scripts"
    git -C "$dir" init -q
    cat >"$dir/scripts/check_untimed_subprocess.py" <<'PY'
import os, sys
with open(os.environ["STUB_OUT"], "w") as f:
    f.write(os.getcwd() + "\n")
    for a in sys.argv[1:]:
        f.write("ARG:" + a + "\n")
sys.exit(int(os.environ.get("STUB_RC", "0")))
PY
}

FAKE="$TMP/fake-repo"
mk_fake_repo "$FAKE"
OUT="$TMP/stub.out"

echo "untimed-subprocess wrapper"

# 1. Exit code and argv (including an argument with a space) pass through.
STUB_OUT="$OUT" STUB_RC=7 QONTINUI_HOOK_REPO_ROOT="$FAKE" \
    bash "$WRAPPER" --list "two words" >/dev/null 2>&1
rc=$?
[ "$rc" -eq 7 ] && ok "exit code 7 preserved" || bad "exit code: want 7, got $rc"
if grep -qx 'ARG:--list' "$OUT" && grep -qx 'ARG:two words' "$OUT" \
    && [ "$(grep -c '^ARG:' "$OUT")" -eq 2 ]; then
    ok "arguments passed through unchanged"
else
    bad "arguments not passed through: $(tr '\n' '|' <"$OUT")"
fi

# 1b. A RELATIVE root still resolves (the wrapper cd's before running).
: >"$OUT"
( cd "$TMP" && STUB_OUT="$OUT" STUB_RC=0 QONTINUI_HOOK_REPO_ROOT="fake-repo"     bash "$WRAPPER" >/dev/null 2>&1 )
rc=$?
[ "$rc" -eq 0 ] && [ -s "$OUT" ] && ok "relative QONTINUI_HOOK_REPO_ROOT resolved"     || bad "relative root: rc=$rc ran=$([ -s "$OUT" ] && echo yes || echo no)"

# 2. No arguments -> the checker receives none.
STUB_OUT="$OUT" STUB_RC=0 QONTINUI_HOOK_REPO_ROOT="$FAKE" bash "$WRAPPER" >/dev/null 2>&1
rc=$?
if [ "$rc" -eq 0 ] && ! grep -q '^ARG:' "$OUT"; then
    ok "no-arg run passes no arguments, exit 0"
else
    bad "no-arg run: rc=$rc argv=$(tr '\n' '|' <"$OUT")"
fi

# 3. Without the env var, the root comes from git — not from the wrapper's own
#    location. Run a COPY of the wrapper living outside any repo, from inside FAKE.
mkdir -p "$TMP/elsewhere" "$FAKE/sub/dir"
cp "$WRAPPER" "$TMP/elsewhere/untimed-subprocess.sh"
: >"$OUT"
( cd "$FAKE/sub/dir" && env -u QONTINUI_HOOK_REPO_ROOT STUB_OUT="$OUT" STUB_RC=0 \
    bash "$TMP/elsewhere/untimed-subprocess.sh" >/dev/null 2>&1 )
rc=$?
if [ "$rc" -eq 0 ] && [ -s "$OUT" ] \
    && [ "$(cd "$(head -1 "$OUT")" && pwd -P)" = "$(cd "$FAKE" && pwd -P)" ]; then
    ok "root resolved via git rev-parse --show-toplevel, checker ran at the root"
else
    bad "git root resolution: rc=$rc cwd=$(head -1 "$OUT" 2>/dev/null)"
fi

# 4. Neither env var nor a git work tree -> refuse with exit 2, never guess.
mkdir -p "$TMP/nogit"
( cd "$TMP/nogit" && env -u QONTINUI_HOOK_REPO_ROOT GIT_CEILING_DIRECTORIES="$TMP" \
    bash "$TMP/elsewhere/untimed-subprocess.sh" >/dev/null 2>&1 )
rc=$?
[ "$rc" -eq 2 ] && ok "no resolvable root -> exit 2" || bad "no root: want 2, got $rc"

# 5. python3 present but unrunnable (the Windows Store stub shape) -> falls back
#    to python.
REAL_PY=""
for c in python3 python; do
    if command -v "$c" >/dev/null 2>&1 && "$c" -c 'import sys; sys.exit(sys.version_info < (3, 11))' >/dev/null 2>&1; then
        REAL_PY="$(command -v "$c")"; break
    fi
done
if [ -z "$REAL_PY" ]; then
    echo "  SKIP python fallback: no runnable python on PATH"
else
    BIN="$TMP/bin"; mkdir -p "$BIN"
    printf '#!/usr/bin/env bash\nexit 9009\n' >"$BIN/python3"
    printf '#!/usr/bin/env bash\nexec "%s" "$@"\n' "$REAL_PY" >"$BIN/python"
    chmod +x "$BIN/python3" "$BIN/python"
    : >"$OUT"
    PATH="$BIN:$PATH" STUB_OUT="$OUT" STUB_RC=3 QONTINUI_HOOK_REPO_ROOT="$FAKE" \
        bash "$WRAPPER" >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 3 ] && [ -s "$OUT" ]; then
        ok "unrunnable python3 -> fell back to python"
    else
        bad "python fallback: rc=$rc ran=$([ -s "$OUT" ] && echo yes || echo no)"
    fi
fi

# 6. The real checker over THIS repo, no arguments (the shim's invocation):
#    it must scan the whole discovered scope and agree with a direct run.
if [ -z "$REAL_PY" ]; then
    echo "  SKIP real-tree run: no runnable python on PATH"
else
    direct_out="$("$REAL_PY" "$REPO_ROOT/scripts/check_untimed_subprocess.py" 2>&1)"; direct_rc=$?
    wrap_out="$(QONTINUI_HOOK_REPO_ROOT="$REPO_ROOT" bash "$WRAPPER" 2>&1)"; wrap_rc=$?
    if [ "$wrap_rc" -eq "$direct_rc" ] && [ "$wrap_out" = "$direct_out" ]; then
        ok "no-arg wrapper run over the real tree matches a direct run (rc=$wrap_rc)"
    else
        bad "real tree: wrapper rc=$wrap_rc vs direct rc=$direct_rc, output differs"
    fi
    roots_out="$(QONTINUI_HOOK_REPO_ROOT="$REPO_ROOT" bash "$WRAPPER" --list-roots 2>&1)"
    if printf '%s\n' "$roots_out" | grep -q '^  src-tauri/src$'; then
        ok "--list-roots passed through and reports src-tauri/src in scope"
    else
        bad "--list-roots did not report src-tauri/src"
    fi
fi

echo "untimed-subprocess wrapper: $pass passed, $failn failed"
[ "$failn" -eq 0 ]
