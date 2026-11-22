#!/usr/bin/env bash
# exec-equivalence-test.sh -- suite for exec-equivalence.py (plan
# 2026-10-10-small-followups-are-recorded-but-nothing-burns-them-down, Phase 2
# step 4).
#
# Hermetic: fixture git repos under a mktemp sandbox (system and global git
# config ignored, so core.autocrlf is off unless a case sets it); coord replaced
# by a loopback fixture (EXEC_EQ_COORD_URL -- the checker's FIXTURE MODE) that
# answers coord's Python route from a table of recorded verdicts, serves
# code_paths, and answers the ownership probe; the bearer is a fake JWT in
# $COORD_DEVICE_JWT claiming the fixture tenant, and the runner rung and every
# mint are off (CTC_NO_RUNNER, CTC_NO_MINT). No network.
#
# Sections (--only <id>[,<id>...]; --list-sections):
#   cli       usage and git errors exit 2; an empty diff exits 0 with files=0
#   paths-*   code_paths, one section per run of fixture blocks:
#             core      a .md outside / inside, undeclared, an incomplete oracle,
#                       added / deleted / mode-changed paths, binary and
#                       no-grammar files inside, an NBSP doctest line
#             doctest   text doctests (r9 F5, r10): added, deleted, Sphinx
#                       directives, NUL-bearing and UTF-16 texts, a link outside
#             fold      fold twins (r16) and twinned ancestors by the fold key
#                       (r17-r20 rows of fold_dir_case)
#             foldwild  twinned ancestors by an unassigned code point or an 8.3
#                       name (r21-r22 rows of fold_dir_case)
#             resolver  the short-name basis, the resolver in a wild directory,
#                       a glob by another spelling, a case-only rename (r22-r23)
#             glob24 glob25 glob26   the folded glob, globset's parse, the 8.3
#                       check across an alternative (r24-r27)
#             twin      the twin pass of a language arm, twin order, names that
#                       are not UTF-8, a link beside a short name (r16-r24)
#             worktree wt25 wt26   the working-tree head (r23-r26)
#             pathspec  a path is never pathspec magic; filters (r27)
#             attr      checkout git attributes in the commits (r28 rows of
#                       attr_case: ident, eol, a changed .gitattributes, the index)
#             attrlocal ...in an ignored working-tree .gitattributes, a replace
#                       ref, the checkout, and local sources (the other r28 rows)
#             attrsys   the system attributes file, local attribute files (r29,
#                       r31)
#             root      a work tree whose name ends in a space (r32)
#   globs     globset semantics
#   include-* non-code files a Rust crate reads at compile time: doc (the doc
#             include), opaque (shapes that may name any file), r7 r9 r10 r11
#             (the rounds that widened it), link (links in the include graph),
#             r14 (untracked spellings, fold twins), resolve (`..` and
#             retargeting), plain (what must stay a plain include), readers
#             (binary .rs, OUT_DIR, compile-time readers)
#   coord     the fixture coord: the parity corpus of coord's ne* proof cases
#             (recorded verdicts) with the profile each path must be sent
#             under, the client-side >>> / shebang / directive refusals,
#             code_paths read under the PROVEN tenant, an unreadable scope, a
#             refuted owner, every bad answer, an unreachable coord (exit 5),
#             no credential, and a non-loopback http url
#   pack      a missing pack and a pack at another version read unknown (needs
#             no real pack)
#   rust ts-core ts-self ts-escape ts-jsx ts-jsx2 shell-core shell-r33 shell-r35
#             the tree-sitter arms. They need tree-sitter-language-pack
#             at the checker's pin: set EXEC_EQ_PYTHON to an interpreter that
#             has it. Without one these cases are UNMEASURED -- counted as
#             neither passed nor failed, and printed -- never skipped silently.
#             That holds for the TS_OK-gated cases in other sections too, and
#             the graded run's isolation control proves every OTHER section
#             alone and reports these (TS_SECTIONS) as UNMEASURED.
#             Under CI (a non-empty $CI other than "false") or with
#             EXEC_EQ_REQUIRE_TS=1 they FAIL instead: the CI job provisions
#             the pin, so there its absence is a fault, not a measurement gap.
#             An explicit EXEC_EQ_REQUIRE_TS=0 overrides $CI (a CI job that
#             runs this suite with no pack on purpose).
#   vendored  lib/ copies are byte-identical to the canonical scripts/lib ones
#   census    the SOURCE_READERS census for the repo this suite runs in: how
#             many of its Python files coord's reader list refuses under each
#             profile, so the profile's cost stays visible
#
# The big sections are split at fixture-block boundaries because every mutant
# re-runs the section(s) its --only names: a mutant narrowed to `paths` whole
# re-ran ~160 fixture repositories to redden on one of them. Each mutant names
# the section(s) measured to redden under it. A block may not lean on another
# block's variables or fixtures; the graded run's mc_only_control proves each
# section green alone (without the pack, each one outside TS_SECTIONS). Helpers
# more than one section uses live at top level.
#
# Prints `executed=<n>`; exits 0 when every executed case passed and at least
# one ran, 1 otherwise.

set -uo pipefail

HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SELF="$HERE/$(basename "${BASH_SOURCE[0]}")"
SUITE_REL=".claude/skills/followup-sweep/exec-equivalence-test.sh"
SUBJECT_REAL="$HERE/exec-equivalence.py"
SUBJECT="${MC_SUBJECT:-$SUBJECT_REAL}"
LIBSRC="$(cd "$HERE/../../../scripts/lib" 2>/dev/null && pwd)" || LIBSRC=""
[ -n "$LIBSRC" ] && [ -r "$LIBSRC/mutation-control.sh" ] || {
  echo "FATAL: this suite runs from a checkout carrying scripts/lib (mutation-control.sh, native-path.sh, fixture-port.sh)"
  exit 2
}
# A mutant copy runs from a staging dir with no lib/ beside it; give it one, so
# a red is the mutation's and never the staging's.
if [ "$SUBJECT" != "$SUBJECT_REAL" ] && [ ! -d "$(dirname "$SUBJECT")/lib" ]; then
  cp -R "$HERE/lib" "$(dirname "$SUBJECT")/lib" || { echo "FATAL: cannot stage lib/ beside $SUBJECT"; exit 2; }
fi

# shellcheck source=../../../scripts/lib/mutation-control.sh
. "$LIBSRC/mutation-control.sh"
# shellcheck source=../../../scripts/lib/fixture-port.sh
. "$LIBSRC/fixture-port.sh"
# The sections whose every case needs the tree-sitter pack (the header's
# "rust ts-core ... shell-r35" row). Without the pack they run no case.
TS_SECTIONS="rust ts-core ts-self ts-escape ts-jsx ts-jsx2 shell-core shell-r33 shell-r35"
mc_only_declare cli \
  paths-core paths-doctest paths-fold paths-foldwild paths-resolver paths-glob24 \
  paths-glob25 paths-glob26 paths-twin paths-worktree paths-wt25 paths-wt26 \
  paths-pathspec paths-attr paths-attrlocal paths-attrsys paths-root \
  globs \
  include-doc include-opaque include-r7 include-r9 include-r10 include-r11 \
  include-link include-r14 include-resolve include-plain include-readers \
  coord pack rust \
  ts-core ts-self ts-escape ts-jsx ts-jsx2 \
  shell-core shell-r33 shell-r35 \
  vendored census
mc_only_parse "$@"

PASS=0; FAIL=0; UNMEASURED=0
ok()  { PASS=$((PASS + 1)); printf '  ok   %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf '  FAIL %s\n' "$1"; [ $# -gt 1 ] && printf '       %s\n' "${2:0:900}"; }
unmeasured() { UNMEASURED=$((UNMEASURED + 1)); printf '  UNMEASURED %s\n' "$1"; }
mc_ok() { ok "$*"; }
mc_bad() { bad "$*"; }

SANDBOX="$(mktemp -d)" || { echo "FATAL: mktemp -d failed"; exit 2; }
FIXTURE_PID=""
cleanup() {
  [ -n "$FIXTURE_PID" ] && kill "$FIXTURE_PID" 2>/dev/null
  rm -rf "$SANDBOX"
}
trap cleanup EXIT
mc_init "$SUITE_REL" "$SANDBOX"

# ---- environment: nothing ambient may answer for the fixture ---------------
unset GIT_ATTR_SOURCE MSYS_NO_PATHCONV COORD_DEVICE_JWT QONTINUI_TENANT_ID QONTINUI_MACHINE_ID \
      COORD_HTTP_URL EXEC_EQ_COORD_URL QONTINUI_BASH GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE
export HOME="$SANDBOX/home" USERPROFILE="$SANDBOX/home" XDG_CONFIG_HOME="$SANDBOX/home/.config"
mkdir -p "$HOME"
export CTC_NO_RUNNER=1 CTC_NO_MINT=1
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_COUNT=3
# The system attributes file is this machine's, like the system config: off
# unless a case turns it on (round 29's rows do).
export GIT_ATTR_NOSYSTEM=1
export GIT_CONFIG_KEY_0=user.name GIT_CONFIG_VALUE_0=exec-eq-test
export GIT_CONFIG_KEY_1=user.email GIT_CONFIG_VALUE_1=exec-eq-test@example.invalid
export GIT_CONFIG_KEY_2=init.defaultBranch GIT_CONFIG_VALUE_2=main

# The interpreter: $EXEC_EQ_PYTHON when set, else the first runnable Python 3
# (a `python3` on Windows is often the Store alias, present and unable to run).
PY=""
if [ -n "${EXEC_EQ_PYTHON:-}" ]; then
  "$EXEC_EQ_PYTHON" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 9) else 1)' >/dev/null 2>&1 \
    && PY="$EXEC_EQ_PYTHON" || { echo "FATAL: EXEC_EQ_PYTHON=$EXEC_EQ_PYTHON is not a runnable Python >= 3.9"; exit 2; }
else
  for _c in python3 python; do
    "$_c" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 9) else 1)' >/dev/null 2>&1 && { PY="$(command -v "$_c")"; break; }
  done
fi
[ -n "$PY" ] || { echo "FATAL: no runnable Python >= 3.9 (python3 or python)"; exit 2; }
SUBJ_N="$(native_path_m "$SUBJECT")"

# The pin is read off the checker itself, so the two can never disagree.
PIN="$(sed -n 's/^TREE_SITTER_PACK_PIN = "\([^"]*\)"$/\1/p' "$SUBJECT_REAL" | head -1)"
[ -n "$PIN" ] || { echo "FATAL: cannot read TREE_SITTER_PACK_PIN from $SUBJECT_REAL"; exit 2; }
TS_VER="$("$PY" -c 'import importlib.metadata as m
try:
    print(m.version("tree-sitter-language-pack"))
except Exception:
    print("absent")' 2>/dev/null | tr -d '\r')"
TS_OK=0
[ "$TS_VER" = "$PIN" ] && TS_OK=1
# In CI (or with EXEC_EQ_REQUIRE_TS=1) a missing pin is a FAILURE, not
# UNMEASURED: the job that runs this suite provisions the pack, so its absence
# there is a broken provisioning step, and a green run over the tree-sitter
# arms' UNMEASURED cases would gate nothing.
# An EXPLICIT EXEC_EQ_REQUIRE_TS (1 or 0) takes precedence over $CI: a CI job
# that deliberately runs this suite without the pack (the Windows
# --probe-undeclared measurement) sets 0, so UNMEASURED stays UNMEASURED there.
TS_REQUIRED=0
case "${EXEC_EQ_REQUIRE_TS:-}" in
  1) TS_REQUIRED=1 ;;
  0) TS_REQUIRED=0 ;;
  *) { [ -n "${CI:-}" ] && [ "${CI:-}" != "false" ]; } && TS_REQUIRED=1 ;;
esac
echo "== exec-equivalence-test: python=$PY tree-sitter-language-pack=${TS_VER:-unknown} (pin $PIN) required=$TS_REQUIRED"
# Every case gated on TS_OK reports through here when the pack is absent, in
# whichever section it sits, so none is skipped without a line saying so.
ts_unmeasured() { # <section>
  local why="$1: tree-sitter-language-pack==$PIN is not importable by $PY (found ${TS_VER:-nothing}); set EXEC_EQ_PYTHON to an interpreter that has it"
  if [ "$TS_REQUIRED" = 1 ]; then
    bad "$why" "required here (CI or EXEC_EQ_REQUIRE_TS=1): a run that cannot measure the tree-sitter arms is a failure, not UNMEASURED"
  else
    unmeasured "$why"
  fi
}
# A TS_SECTIONS name that is not a declared section is a typo or a rename the
# list missed, never an exemption: checked on every run, pack or not.
for _s in $TS_SECTIONS; do
  case " $MC_ONLY_SECTIONS " in
    *" $_s "*) ;;
    *) bad "TS_SECTIONS names $_s, which is not a declared section" ;;
  esac
done

TENANT="aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
DEVICE="11111111-2222-4333-8444-555555555555"
FAKE_JWT="$("$PY" -c 'import base64, json, sys, time
enc = lambda d: base64.urlsafe_b64encode(json.dumps(d).encode()).decode().rstrip("=")
print(enc({"alg": "none", "typ": "JWT"}) + "." + enc({"exp": int(time.time()) + 3600, "tenant_id": sys.argv[1], "device_id": sys.argv[2]}) + ".c2ln")' "$TENANT" "$DEVICE" | tr -d '\r')"
[ -n "$FAKE_JWT" ] || { echo "FATAL: could not build the fixture JWT"; exit 2; }

# ---- helpers -----------------------------------------------------------------
mkrepo() { # <dir> -- a repo whose origin names acme/widget (never fetched)
  rm -rf "$1"; mkdir -p "$1"
  git init -q "$1" && git -C "$1" config core.autocrlf false \
    && git -C "$1" config core.filemode false \
    && git -C "$1" remote add origin https://github.com/acme/widget.git
}
wr() { mkdir -p "$(dirname "$1")"; cat > "$1"; }   # stdin -> file, byte-exact
cm() { git -C "$1" add -A && git -C "$1" commit -q -m "$2"; }

PYFLAGS=()
run_check() { # <repo> [args...] -> OUT, ERR, RC
  local repo="$1"; shift
  RC=0
  OUT="$("$PY" ${PYFLAGS[@]+"${PYFLAGS[@]}"} "$SUBJ_N" --repo-root "$(native_path_m "$repo")" "$@" 2>"$SANDBOX/last.err")" || RC=$?
  OUT="${OUT//$'\r'/}"
  ERR="$(cat "$SANDBOX/last.err" 2>/dev/null)"
}
expect_rc() { # <case> <want>
  if [ "$RC" = "$2" ]; then ok "$1"; else bad "$1" "expected exit $2, got $RC; stderr: ${ERR:0:400}; stdout: ${OUT:0:400}"; fi
}
expect_file() { # <case> <path> <verdict> [<reason prefix>]
  local line v r
  line="$(printf '%s\n' "$OUT" | awk -F'\t' -v p="$2" '$2 == p { print; exit }')"
  if [ -z "$line" ]; then bad "$1" "no verdict line for $2 in: ${OUT:0:600} / ${ERR:0:300}"; return; fi
  v="${line%%$'\t'*}"; r="${line##*$'\t'}"
  if [ "$v" != "$3" ]; then bad "$1" "$2: expected $3, got $v ($r)"; return; fi
  if [ -n "${4:-}" ]; then
    case "$r" in "$4"*) ;; *) bad "$1" "$2: expected reason $4*, got $r"; return ;; esac
  fi
  ok "$1"
}
expect_out() { # <case> <substring>
  case "$OUT" in *"$2"*) ok "$1" ;; *) bad "$1" "[$2] not in stdout: ${OUT:0:500} / ${ERR:0:300}" ;; esac
}
expect_err() { # <case> <substring>
  case "$ERR" in *"$2"*) ok "$1" ;; *) bad "$1" "[$2] not in stderr: ${ERR:0:500}" ;; esac
}

# ---- the fixture coord ---------------------------------------------------------
FIX_DIR="$SANDBOX/fixture"
ensure_fixture() {
  [ -n "$FIXTURE_PID" ] && return 0
  mkdir -p "$FIX_DIR"
  printf '{}' > "$FIX_DIR/cfg.json"
  printf '{}' > "$FIX_DIR/table.json"
  wr "$FIX_DIR/fixture_coord.py" <<'PYEOF'
import base64, hashlib, json, os, sys, threading, urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT_FILE, DIR = sys.argv[1], sys.argv[2]
LOCK = threading.Lock()

def sha(text):
    return hashlib.sha256(text.encode("utf-8", "surrogatepass")).hexdigest()

def load(name):
    try:
        with open(os.path.join(DIR, name), encoding="utf-8") as fh:
            return json.load(fh)
    except (OSError, ValueError):
        return {}

def log(rec):
    with LOCK:
        with open(os.path.join(DIR, "requests.log"), "a", encoding="utf-8") as fh:
            fh.write(json.dumps(rec) + "\n")

def bearer_tenant(header):
    try:
        seg = header.split(" ", 1)[1].split(".")[1]
        seg += "=" * (-len(seg) % 4)
        return json.loads(base64.urlsafe_b64decode(seg)).get("tenant_id", "")
    except Exception:
        return ""

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, code, obj=None, raw=None):
        body = raw if raw is not None else json.dumps(obj).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def authed(self):
        return self.headers.get("Authorization", "").startswith("Bearer ")

    def do_POST(self):
        if self.path != "/coord/exec-equivalence/python":
            return self.reply(404, {"error": "no_route"})
        if not self.authed():
            return self.reply(403, {"error": "auth_required"})
        size = int(self.headers.get("Content-Length") or 0)
        try:
            req = json.loads(self.rfile.read(size).decode("utf-8"))
        except ValueError:
            return self.reply(400, {"error": "bad_json"})
        cfg = load("cfg.json")
        path, profile = req.get("path"), req.get("profile")
        log({"route": "python", "path": path, "profile": profile})
        mode = cfg.get("python_mode", "table")
        if mode == "http_404":
            return self.reply(404, {"error": "no_route"})
        if mode == "http_422":
            return self.reply(422, {"error": "unknown_profile"})
        if mode == "malformed":
            return self.reply(200, raw=b"this is not json")
        row = load("table.json").get("%s|%s|%s" % (path, sha(req.get("base", "")), sha(req.get("head", ""))))
        if row is None:
            return self.reply(500, {"error": "fixture_has_no_row"})
        verdict = row.get(profile, row.get("any"))
        if not isinstance(verdict, bool):
            return self.reply(500, {"error": "fixture_row_has_no_verdict_for_profile"})
        echoed = "not-" + profile if mode == "wrong_profile" else profile
        return self.reply(200, {"non_executable": verdict, "profile": echoed})

    def do_GET(self):
        url = urllib.parse.urlsplit(self.path)
        cfg = load("cfg.json")
        if url.path == "/coord/post-merge-followup-scope":
            if not self.authed():
                return self.reply(403, {"error": "auth_required"})
            repo = urllib.parse.parse_qs(url.query).get("repo", [""])[0]
            log({"route": "scope", "repo": repo, "tenant": bearer_tenant(self.headers.get("Authorization", ""))})
            status = cfg.get("scope_status", 200)
            if status != 200:
                return self.reply(status, {"error": "preference_unreadable"})
            return self.reply(200, {"domain": "post_merge_followup_scope", "repo": repo, "scope": "code_only",
                                    "code_paths": cfg.get("code_paths", []), "resolved_scope": "repo",
                                    "mode": "shadow"})
        parts = url.path.split("/")
        if len(parts) == 5 and parts[1] == "pr-merge" and parts[3] == "0" and parts[4] == "author-session":
            repo = urllib.parse.unquote(parts[2])
            log({"route": "ownership", "repo": repo})
            if repo not in cfg.get("owned", []):
                return self.reply(404, {"error": "not_found"})
            return self.reply(200, {"repo": repo, "pr_number": 0})
        return self.reply(404, {"error": "no_route"})

srv = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
tmp = PORT_FILE + ".tmp"
with open(tmp, "w") as fh:
    fh.write("%d\n" % srv.server_address[1])
    fh.flush()
os.replace(tmp, PORT_FILE)
srv.serve_forever()
PYEOF
  fixture_launch $$ -- "$PY" "$(native_path_m "$FIX_DIR/fixture_coord.py")" "$(native_path_m "$FIX_DIR/port")" "$(native_path_m "$FIX_DIR")" \
    >"$FIX_DIR/fixture.log" 2>&1 || {
    bad "the fixture coord launched under fixture_launch" "$(tail -n 3 "$FIX_DIR/fixture.log" 2>/dev/null)"
    return 1
  }
  FIX_PORT="$(fixture_port_wait "$FIXTURE_PID" "$FIX_DIR/port" "$FIX_DIR/fixture.log")" || {
    bad "the fixture coord started and reported its port"
    return 1
  }
  FIX_URL="http://127.0.0.1:$FIX_PORT"
}
set_cfg() { printf '%s' "$1" > "$FIX_DIR/cfg.json"; }
clear_log() { : > "$FIX_DIR/requests.log"; }

# ---- section: cli --------------------------------------------------------------
if mc_sec cli; then
  echo "== cli"
  R="$SANDBOX/cli"; mkrepo "$R"
  printf 'x\n' | wr "$R/a.txt"; cm "$R" base
  run_check "$R"
  expect_rc "cli: a missing --base is a usage error (exit 2)" 2
  run_check "$R" --base no-such-ref
  expect_rc "cli: a --base that names no commit is exit 2" 2
  mkdir -p "$SANDBOX/plain"
  GIT_CEILING_DIRECTORIES="$(native_path_m "$SANDBOX")" run_check "$SANDBOX/plain" --base HEAD
  expect_rc "cli: a --repo-root outside any work tree is exit 2" 2
  run_check "$R" --base HEAD --code-paths '{not json'
  expect_rc "cli: --code-paths that is not JSON is exit 2" 2
  run_check "$R" --base HEAD --code-paths '"src/**"'
  expect_rc "cli: --code-paths that is not an array of strings is exit 2" 2
  run_check "$R" --help
  expect_rc "cli: --help exits 2, never the all-non_executable 0" 2
  run_check "$R" --base HEAD --head HEAD
  expect_rc "cli: an empty diff exits 0" 0
  expect_out "cli: an empty diff says files=0 rather than nothing" "files=0"
  run_check "$R" --base HEAD --head HEAD --json
  if printf '%s' "$OUT" | "$PY" -c 'import json, sys; d = json.load(sys.stdin); sys.exit(0 if d["verdict"] == "non_executable" and d["exit_code"] == 0 and d["files"] == [] and d["tree_sitter"]["pin"] else 1)'; then
    ok "cli: --json is one document carrying verdict, exit_code, files and the pin"
  else
    bad "cli: --json is one document carrying verdict, exit_code, files and the pin" "${OUT:0:400}"
  fi
fi

# ---- section: paths-core ---------------------------------------------------------
if mc_sec paths-core; then
  echo "== paths-core"
  R="$SANDBOX/paths"; mkrepo "$R"
  printf '# A\n\nold prose\n' | wr "$R/docs/a.md"
  printf 'Do the thing.\n' | wr "$R/src/prompt.md"
  printf 'gone\n' | wr "$R/docs/old.md"
  printf 'x\n' | wr "$R/docs/exec.txt"
  printf '#!/bin/bash\necho hi\n' | wr "$R/run.sh"
  printf 'Intro\n\n>>> 1 + 1\n2\n\nTail.\n' | wr "$R/docs/guide.md"
  printf 'Intro\n\n>>> 1 + 1\n2\n\nTail.\n' | wr "$R/docs/guide2.md"
  printf 'PNG\0\1\2' | wr "$R/src/logo.png"
  printf 'a: 1\n' | wr "$R/src/conf.yaml"
  printf '{"a": 1}\n' | wr "$R/src/data.json"
  cm "$R" base; BASE_P="$(git -C "$R" rev-parse HEAD)"
  printf '# A\n\nnew prose\n' | wr "$R/docs/a.md"
  printf 'Do the other thing.\n' | wr "$R/src/prompt.md"
  rm -f "$R/docs/old.md"
  printf 'Intro\n\n>>> 1 + 1\n3\n\nTail.\n' | wr "$R/docs/guide.md"
  printf 'Intro changed\n\n>>> 1 + 1\n2\n\nTail.\n' | wr "$R/docs/guide2.md"
  printf 'PNG\0\1\3' | wr "$R/src/logo.png"
  printf 'a: 1  # c\n' | wr "$R/src/conf.yaml"
  printf '{"a": 2}\n' | wr "$R/src/data.json"
  printf 'new\n' | wr "$R/docs/new.md"
  printf 'new\n' | wr "$R/src/new.md"
  printf 'fn n() {}\n' | wr "$R/src/lib/new.rs"
  git -C "$R" add -A && git -C "$R" update-index --chmod=+x docs/exec.txt run.sh && git -C "$R" commit -q -m head
  run_check "$R" --base "$BASE_P" --head HEAD --code-paths '["src/**"]'
  expect_rc "paths: a batch holding an executable file exits 4" 4
  expect_file "paths: a .md outside code_paths is non_executable" docs/a.md non_executable outside_code_paths
  expect_file "paths: a .md inside code_paths is executable (a command prompt is code)" src/prompt.md executable inside_code_paths:no_inert_sublanguage
  expect_file "paths: a deleted file outside code_paths is non_executable" docs/old.md non_executable outside_code_paths
  expect_file "paths: a mode change outside code_paths is non_executable" docs/exec.txt non_executable outside_code_paths
  expect_file "paths: a mode change on a shell script is executable" run.sh executable mode_changed
  expect_file "paths: a changed paragraph holding >>> is a text doctest" docs/guide.md executable doctest_marker_in_text
  expect_file "paths: a prose change beside an untouched >>> paragraph is non_executable" docs/guide2.md non_executable outside_code_paths
  expect_file "paths: a binary file inside code_paths is executable" src/logo.png executable inside_code_paths:binary
  expect_file "paths: a no-grammar text file inside code_paths is unknown" src/conf.yaml unknown inside_code_paths:no_grammar:.yaml
  expect_file "paths: a JSON file inside code_paths is executable (no comments)" src/data.json executable inside_code_paths:no_inert_sublanguage
  expect_file "paths: an added file outside code_paths is non_executable" docs/new.md non_executable outside_code_paths
  expect_file "paths: an added file inside code_paths is executable" src/new.md executable inside_code_paths:added
  expect_file "paths: an added source file is executable whatever code_paths says" src/lib/new.rs executable added

  run_check "$R" --base "$BASE_P" --head HEAD --code-paths '[]'
  expect_file "paths: no declared code_paths makes a non-code file unknown" docs/a.md unknown code_paths_undeclared
  expect_file "paths: a source file is judged by its arm even with no code_paths" src/lib/new.rs executable added
  run_check "$R" --base "$BASE_P" --head HEAD --code-paths '["src/[", "src/**"]'
  expect_file "paths: a glob that does not compile makes a non-match unknown" docs/a.md unknown code_paths_oracle_incomplete
  expect_file "paths: a match on an incomplete oracle is still conclusive" src/prompt.md executable inside_code_paths

  U="$SANDBOX/undeclared"; mkrepo "$U"
  printf 'one\n' | wr "$U/docs/a.md"; cm "$U" base; BASE_U="$(git -C "$U" rev-parse HEAD)"
  printf 'two\n' | wr "$U/docs/a.md"; cm "$U" head
  run_check "$U" --base "$BASE_U" --head HEAD --code-paths '[]'
  expect_rc "paths: unknown and nothing executable exits 5" 5
  expect_out "paths: the summary names the unknown verdict" "verdict=unknown"
  run_check "$U" --base "$BASE_U" --head HEAD --code-paths '["src/**"]'
  expect_rc "paths: every file non_executable exits 0" 0

  # Round-8 td2: doctest ends an example's expected output only at a line of
  # SPACES; a line holding an NBSP is output, not a paragraph break.
  N="$SANDBOX/nbsp"; mkrepo "$N"
  printf 'Example.\n\n>>> print("x\\n\302\240 \\ny")\nx\n\302\240 \ny\n' | wr "$N/docs/guide.txt"
  cm "$N" base; BASE_N="$(git -C "$N" rev-parse HEAD)"
  printf 'Example.\n\n>>> print("x\\n\302\240 \\ny")\nx\n\302\240 \nz\n' | wr "$N/docs/guide.txt"
  cm "$N" head
  run_check "$N" --base "$BASE_N" --head HEAD --code-paths '["src/**"]'
  expect_file "paths: r8 td2: an NBSP line does not end a text doctest's expected output" docs/guide.txt executable doctest_marker_in_text
fi

# ---- section: paths-doctest ------------------------------------------------------
if mc_sec paths-doctest; then
  echo "== paths-doctest"
  # Round-9 F5: a text doctest is never proven inert by lying outside
  # code_paths. Added and deleted texts carrying a marker are executable; any
  # edit to a text carrying a Sphinx doctest directive is unknown, because a
  # `testsetup` with no `>>>` feeds an example far from it.
  D="$SANDBOX/textdoctest"; mkrepo "$D"
  printf 'Gone.\n\n>>> 1 + 1\n2\n' | wr "$D/docs/gone.txt"
  printf '.. testsetup::\n\n   x = 1\n\nProse.\n\n>>> x\n1\n' | wr "$D/docs/setup.rst"
  printf 'plain\n' | wr "$D/docs/plain.txt"
  cm "$D" base; BASE_D="$(git -C "$D" rev-parse HEAD)"
  git -C "$D" rm -q docs/gone.txt
  printf 'Added.\n\n>>> 1 + 1\n2\n' | wr "$D/docs/added.txt"
  printf '.. testcode::\n\n   print(1)\n' | wr "$D/docs/added.rst"
  printf '.. testsetup::\n\n   x = 2\n\nProse.\n\n>>> x\n1\n' | wr "$D/docs/setup.rst"
  printf 'Added plain.\n' | wr "$D/docs/added-plain.txt"
  printf 'plainer\n' | wr "$D/docs/plain.txt"
  cm "$D" head
  run_check "$D" --base "$BASE_D" --head HEAD --code-paths '["src/**"]'
  expect_file "paths: r9 F5: an added text holding >>> is executable" docs/added.txt executable doctest_marker_in_text:added:prompt
  expect_file "paths: r9 F5: a deleted text holding >>> is executable" docs/gone.txt executable doctest_marker_in_text:deleted:prompt
  expect_file "paths: r9 F5: an added text holding a Sphinx testcode directive is executable" docs/added.rst executable doctest_marker_in_text:added:sphinx_doctest_directive
  expect_file "paths: r9 F5: an edit to a testsetup block with no >>> is unknown" docs/setup.rst unknown doctest_marker_in_text:sphinx_doctest_directive
  expect_file "paths: r9 F5: an added text with no marker stays non_executable" docs/added-plain.txt non_executable outside_code_paths
  expect_file "paths: r9 F5: an edited text with no marker stays non_executable" docs/plain.txt non_executable outside_code_paths

  # Round 10: the siblings of F5. B2: docutils lowercases a directive name, so
  # `.. TESTSETUP::` is `testsetup`. G1: a NUL-bearing text is still read by
  # doctest, so no binary gate decides an added one. N1: a UTF-16 text hides
  # its `>>>` from a byte search. B4: a link (or a submodule pointer) outside
  # code_paths names content a collector walks into.
  R="$SANDBOX/textdoctest-r10"; mkrepo "$R"
  printf 'Intro.\n\n.. TESTSETUP::\n\n   x = 1\n\nProse.\n\n.. DOCTEST::\n\n   >>> x\n   1\n' | wr "$R/docs/upper.rst"
  printf 'Doc.\n\n>>> 1\n1\n' | iconv -f UTF-8 -t UTF-16 | wr "$R/docs/wide.txt"
  printf 'Doc.\n\n>>> 1 + 1\n2\n' | wr "$R/tests/real.txt"
  cm "$R" base; BASE_R="$(git -C "$R" rev-parse HEAD)"
  printf 'Intro.\n\n.. TESTSETUP::\n\n   x = 2\n\nProse.\n\n.. DOCTEST::\n\n   >>> x\n   1\n' | wr "$R/docs/upper.rst"
  printf '.. TestCode::\n\n   raise SystemExit(1)\n' | wr "$R/docs/added-upper.rst"
  printf 'New \000.\n\n>>> 1/0\n' | wr "$R/docs/nul.txt"
  printf 'Doc.\n\n>>> 1\n2\n' | iconv -f UTF-8 -t UTF-16 | wr "$R/docs/wide.txt"
  printf 'New.\n\n>>> 1/0\n' | iconv -f UTF-8 -t UTF-16LE | wr "$R/docs/wide-added.txt"
  ln -s ../tests/real.txt "$R/docs/extra.txt"
  cm "$R" head
  run_check "$R" --base "$BASE_R" --head HEAD --code-paths '["src/**"]'
  expect_file "paths: r10 B2: an edit to an upper-case TESTSETUP block is unknown" docs/upper.rst unknown doctest_marker_in_text:sphinx_doctest_directive
  expect_file "paths: r10 B2: an added text holding a mixed-case TestCode directive is executable" docs/added-upper.rst executable doctest_marker_in_text:added:sphinx_doctest_directive
  expect_file "paths: r10 G1: an added NUL-bearing text holding >>> is executable" docs/nul.txt executable doctest_marker_in_text:added:prompt
  expect_file "paths: r10 N1: an edit to a UTF-16 text doctest is unknown" docs/wide.txt unknown doctest_marker_in_text:wide_encoding
  expect_file "paths: r10 N1: an added BOM-less UTF-16LE text doctest is executable" docs/wide-added.txt executable doctest_marker_in_text:added:wide_encoding
  expect_file "paths: r10 B4: an added symlink outside code_paths is unknown" docs/extra.txt unknown symlink_outside_code_paths
fi

# ---- fold-dir rows (top level: paths-fold and paths-foldwild run them) -----------
# Round 17's F1: a twin at ANY ancestor counts. With `config/` and
# `Config/` both tracked, `Config/app.yaml` IS `config/app.yaml` on NTFS /
# APFS though no file is spelled so; and the fold key over-folds U+0131
# DOTLESS I into `i`, as a simple-uppercase table does.
# Round 18's F1: the key is canonical caseless matching. `ΐ` (U+0390) and
# `Ϊ́` (U+03AA U+0301) are one name only if the key normalises AFTER
# casefold. `confìg` and `confı̀g`, and `ᾷ` (U+1FB7) and `ᾼ͂` (U+1FBC
# U+0342), from round 18, pin the U+0131 map and where an iota sits.
# Round 19's F1: U+0345 folds to an iota, a starter. D145 moves it behind
# the other marks first; ext4 and simple uppercase fold it where it
# stands. So `conf` U+0345 U+0301 `g` is `confίg` (U+03AF) on ext4, `a`
# U+0345 U+0301 is `a` U+03B9 U+0301 on ext4 and under simple uppercase,
# and two of them are `confιίg`.
# Round 20's F1: ext4/f2fs casefolding drops Default_Ignorable code points,
# so `con` U+200D `fig` and `con` U+00AD `fig` are `config` there, and
# `a` U+0345 U+200D U+0301 is `aί`. The key drops them FIRST: dropped at
# the end, U+0345 and U+0301 stay apart. Round 20's F2: the marks around
# an iota are sorted too, so `a` U+0301 U+0345 U+0316 is `a` U+0301 U+03B9
# U+0316, as on ext4 and under simple uppercase.
# Round 21's F1: a later Unicode joins names only through a code point
# unassigned today: xnu's casefold, at 16.0, makes `confꟋg` (U+A7CB) and
# `confɤg` (U+0264) one, and Garay U+10D50 and U+10D70 one. Round 21's 8.3
# gap: on a volume with short names, `CONFIG~1` can BE `CONFIGURATION`, in
# either direction and in any case; `config~old` is no short name and
# stays apart. Round 22's F2: such a name makes what is under it unknown,
# so no twin spelling is composed and the path is unknown outright, at
# any depth (`CONFIG~1/Sub` against `configuration/sub`, two wild levels,
# either direction). Where the interpreter's Unicode assigns the code
# points, the fold key joins the names itself, so those reasons are read
# off the interpreter. Round 22's F3: Windows keeps `~` in a short name's
# base (`a~bcdefgh` gets `A~BCDE~1`, `~backup_dir` gets `~BACKU~1`) and
# drops a trailing dot (`CONFIG~1.`). Round 23's F2: the glob goes through
# the same rules, so where the glob's own spelling folds to the changed
# path the oracle holds it (`inside_code_paths`), and a wild literal in the
# glob makes paths beside it unknown (`code_paths_wild`). `a?◌̖/app.yaml`
# holds `a◌́◌̖/` exactly, and a fold reorders the marks of `a◌̖◌́/` past the
# `?`; since round 25 the folded glob drops the marks at the `?`'s edge and
# holds it too (`twin-reorder`; the twin pass's code_paths arm is pinned by
# files a language arm judged, below).
fold_dir_case() { # <variant>
  local V="$1"
  T="$SANDBOX/r17-fold-dir-$V"; mkrepo "$T"
  R=r18; FN=F1; C=config; RS=; GL=; EV=unknown; WHAT="a file under a twinned ancestor of a code_paths directory is unknown"
  case "$V" in
    twin-reorder) R=r24; FN=F1; C="$(printf 'a\314\201\314\226')"; D="$(printf 'a\314\226\314\201')"; GL="$(printf 'a?\314\226/app.yaml')" ;;
    case) R=r17; D=Config ;;
    dotless) R=r17; D="$(printf 'conf\304\261g')" ;;
    greek) C="$(printf '\316\220')"; D="$(printf '\316\252\314\201')" ;;
    dotless-mark) C="$(printf 'conf\303\254g')"; D="$(printf 'conf\304\261\314\200g')" ;;
    ypogegrammeni) C="$(printf '\341\276\267')"; D="$(printf '\341\276\274\315\202')" ;;
    tonos) R=r19; C="$(printf 'conf\316\257g')"; D="$(printf 'conf\315\205\314\201g')" ;;
    iota-mark) R=r19; C="$(printf 'a\316\271\314\201')"; D="$(printf 'a\315\205\314\201')" ;;
    double-iota) R=r19; C="$(printf 'conf\316\271\316\257g')"; D="$(printf 'conf\315\205\315\205\314\201g')" ;;
    zwj) R=r20; D="$(printf 'con\342\200\215fig')" ;;
    shy) R=r20; D="$(printf 'con\302\255fig')" ;;
    ignorable-iota) R=r20; C="$(printf 'a\316\271\314\201')"; D="$(printf 'a\315\205\342\200\215\314\201')" ;;
    mark-order) R=r20; FN=F2; C="$(printf 'a\314\201\316\271\314\226')"; D="$(printf 'a\314\201\315\205\314\226')" ;;
    unassigned) R=r21; C="$(printf 'conf\311\244g')"; D="$(printf 'conf\352\237\213g')" ;;
    unassigned-reverse) R=r21; C="$(printf 'conf\352\237\213g')"; D="$(printf 'conf\311\244g')" ;;
    garay) R=r21; C="$(printf '\360\220\265\260')"; D="$(printf '\360\220\265\220')" ;;
    short-name) R=r21; FN=8.3; C=configuration; D='CONFIG~1'; RS='short_name:CONFIG~1' ;;
    short-name-reverse) R=r21; FN=8.3; C='CONFIG~1'; D=configuration; RS='code_paths_wild:CONFIG~1/**' ;;
    short-name-lower) R=r21; FN=8.3; C=configuration; D='config~1'; RS='short_name:config~1' ;;
    short-name-control) R=r21; FN=8.3; D='config~old'; EV=non_executable; RS=outside_code_paths; WHAT="a tilde name that is not 8.3-shaped is no alias" ;;
    wild-then-case) R=r22; FN=F2; C=configuration/sub; D='CONFIG~1/Sub'; RS='short_name:CONFIG~1' ;;
    wild-wild) R=r22; FN=F2; C=configuration/subdirectory; D='CONFIG~1/SUBDIR~1'; RS='short_name:CONFIG~1' ;;
    unassigned-then-case) R=r22; FN=F2; C="$(printf 'conf\311\244g')/sub"; D="$(printf 'conf\352\237\213g')/Sub" ;;
    wild-reverse-case) R=r22; FN=F2; C='CONFIG~1/Sub'; D=configuration/sub; RS='code_paths_wild:CONFIG~1/Sub/**' ;;
    tilde-basis) R=r22; FN=F3; C='a~bcdefgh'; D='A~BCDE~1'; RS='short_name:A~BCDE~1' ;;
    leading-tilde) R=r22; FN=F3; C='~backup_dir'; D='~BACKU~1'; RS='short_name:~BACKU~1' ;;
    trailing-dot) R=r22; FN=F3; C=configuration; D='CONFIG~1.'; RS='short_name:CONFIG~1.' ;;
  esac
  printf 'a: 1\n' | wr "$T/$C/settings.yaml"; printf 'a: 1\n' | wr "$T/$D/app.yaml"
  cm "$T" base; B_T="$(git -C "$T" rev-parse HEAD)"
  printf 'a: 2\n' | wr "$T/$D/app.yaml"
  cm "$T" head
  case "$V" in unassigned*|garay)
    RS="$("$PY" -c 'import sys, unicodedata
c, d = sys.argv[1:3]
cn = lambda s: any(unicodedata.category(x) == "Cn" for x in s)
print("code_paths_wild:" + c + "/**" if cn(c) else "unassigned_codepoint:" + d.split("/")[0] if cn(d) else "inside_code_paths:no_grammar:.yaml")' "$C" "$D")" ;;
  esac
  # Where the glob's own spelling folds to the changed path's, the folded
  # glob holds it (round 23).
  [ -n "$RS" ] || RS="inside_code_paths:no_grammar:.yaml"
  [ -n "$GL" ] || GL="$C/**"
  run_check "$T" --base "$B_T" --head HEAD --code-paths "[\"$GL\"]"
  expect_file "paths: $R $FN ($V): $WHAT" "$D/app.yaml" "$EV" "$RS"
}

# ---- section: paths-fold ---------------------------------------------------------
if mc_sec paths-fold; then
  echo "== paths-fold"
  # Round 16's F1: membership by exact spelling consults the fold twins. On
  # NTFS / APFS `Config/app.yaml` IS `config/app.yaml`, so an edit to it is
  # not proven outside code_paths by its spelling; and a twin that a
  # language arm would read (a python shebang) is not proven inert either.
  T="$SANDBOX/r16-fold-twin"; mkrepo "$T"
  printf 'a: 1\n' | wr "$T/config/app.yaml"; printf 'a: 1\n' | wr "$T/Config/app.yaml"
  printf '#!/usr/bin/env python3\nprint(1)\n' | wr "$T/bin/tool"; printf 'notes\n' | wr "$T/bin/TOOL"
  printf 'plain\n' | wr "$T/docs/plain.md"
  cm "$T" base; B_T="$(git -C "$T" rev-parse HEAD)"
  printf 'a: 2\n' | wr "$T/Config/app.yaml"
  printf 'notes, edited\n' | wr "$T/bin/TOOL"
  printf 'plainer\n' | wr "$T/docs/plain.md"
  cm "$T" head
  run_check "$T" --base "$B_T" --head HEAD --code-paths '["config/**"]'
  # Since round 23 the glob is folded too, so the oracle itself holds
  # `Config/app.yaml`; the twin pass's own code_paths arm is pinned by the
  # fold loop's `twin-negated` row below.
  expect_file "paths: r16 F1: a file whose fold twin is inside code_paths is unknown" Config/app.yaml unknown inside_code_paths:no_grammar:.yaml
  expect_file "paths: r16 F1: a file whose fold twin a language arm reads is unknown" bin/TOOL unknown fold_twin:bin/tool
  expect_file "paths: r16 F1: a file with no fold twin is untouched" docs/plain.md non_executable outside_code_paths
  for V in twin-reorder case dotless greek dotless-mark ypogegrammeni tonos iota-mark double-iota zwj shy ignorable-iota mark-order; do fold_dir_case "$V"; done
fi

# ---- section: paths-foldwild -----------------------------------------------------
if mc_sec paths-foldwild; then
  echo "== paths-foldwild"
  for V in unassigned unassigned-reverse garay short-name short-name-reverse short-name-lower short-name-control wild-then-case wild-wild unassigned-then-case wild-reverse-case tilde-basis leading-tilde trailing-dot; do fold_dir_case "$V"; done
fi

# ---- section: paths-resolver -----------------------------------------------------
if mc_sec paths-resolver; then
  echo "== paths-resolver"
  # Round 22's F3: an Office-style `~$` base keeps its tilde in the short
  # name, extension included: `~$SETT~1.YAM` can BE `~$settings.yaml`.
  T="$SANDBOX/r22-office-basis"; mkrepo "$T"
  printf 'a: 1\n' | wr "$T/docs/~\$settings.yaml"; printf 'a: 1\n' | wr "$T/docs/~\$SETT~1.YAM"
  cm "$T" base; B_T="$(git -C "$T" rev-parse HEAD)"
  printf 'a: 2\n' | wr "$T/docs/~\$SETT~1.YAM"
  cm "$T" head
  run_check "$T" --base "$B_T" --head HEAD --code-paths '["docs/~$settings.yaml"]'
  expect_file "paths: r22 F3 (office-basis): a short name whose base keeps its tilde is unknown" 'docs/~$SETT~1.YAM' unknown 'short_name:docs/~$SETT~1.YAM'
  # Round 22's F1: the resolver judges where it starts, at every level, and
  # so every directory `..` re-enters. An include or a link read from inside
  # `config~1/` (which may BE `configuration/`), or from a directory with an
  # unassigned code point, is not followed. `rs-up`, a link `..` out of
  # `config~1/sub`, appends no component, so only the start check sees it.
  for V in rs-include rs-unassigned rs-dotdot rs-link rs-up; do
    T="$SANDBOX/r22-resolver-$V"; mkrepo "$T"
    WD='config~1'; OD=configuration
    [ "$V" = rs-unassigned ] && { WD="$(printf 'conf\352\237\213g')"; OD="$(printf 'conf\311\244g')"; }
    printf 'old\n' | wr "$T/$WD/x.txt"; printf 'one\n' | wr "$T/$OD/x.txt"
    case "$V" in
      rs-include|rs-unassigned) printf 'pub const X: &str = include_str!("x.txt");\n' | wr "$T/$WD/lib.rs"; WHY="included_by_rust_opaque:$WD/lib.rs:unresolved_path:x.txt" ;;
      rs-dotdot) printf 'pub const X: &str = include_str!("../x.txt");\n' | wr "$T/$WD/sub/lib.rs"; WHY="included_by_rust_opaque:$WD/sub/lib.rs:unresolved_path:../x.txt" ;;
      rs-link) ln -s x.txt "$T/$WD/l"; WHY="aliased_by_symlink:$WD/l" ;;
      rs-up) printf 's\n' | wr "$T/$WD/sub/s.txt"; ln -s .. "$T/$WD/sub/up"; WHY="aliased_by_symlink:$WD/sub/up" ;;
    esac
    cm "$T" base; B_T="$(git -C "$T" rev-parse HEAD)"
    printf 'two\n' | wr "$T/$OD/x.txt"
    cm "$T" head
    run_check "$T" --base "$B_T" --head HEAD --code-paths '["src/**"]'
    expect_file "paths: r22 F1 ($V): a file a read from inside a wild directory may name is unknown" "$OD/x.txt" unknown "$WHY"
  done
  # Round 23's F2: a code_paths glob spelled as a fold variant, an 8.3 short
  # name, an unassigned code point, with a trailing dot, or in NFC against
  # an NFD tree names the tracked directory no glob spells exactly.
  for V in g-case g-short g-unassigned g-dot g-nfc; do
    T="$SANDBOX/r23-glob-$V"; mkrepo "$T"
    D=config; GL='Config/**'; RS=inside_code_paths:no_grammar:.yaml
    case "$V" in
      g-short) D=configuration; GL='CONFIG~1/**'; RS='code_paths_wild:CONFIG~1/**' ;;
      g-unassigned) D="$(printf 'conf\311\244g')"; GL="$(printf 'conf\352\237\213g')/**"
        RS="$("$PY" -c 'import sys, unicodedata; print("code_paths_wild:" + sys.argv[1] if any(unicodedata.category(c) == "Cn" for c in sys.argv[1]) else "inside_code_paths:no_grammar:.yaml")' "$GL")" ;;
      g-dot) GL='config./**' ;;
      g-nfc) D="$(printf 'cafe\314\201')"; GL="$(printf 'caf\303\251')/**" ;;
    esac
    printf 'a: 1\n' | wr "$T/$D/app.yaml"; printf 'r\n' | wr "$T/README"
    cm "$T" base; B_T="$(git -C "$T" rev-parse HEAD)"
    printf 'a: 2\n' | wr "$T/$D/app.yaml"
    cm "$T" head
    run_check "$T" --base "$B_T" --head HEAD --code-paths "[\"$GL\"]"
    expect_file "paths: r23 F2 ($V): a glob that names the directory by another spelling holds it" "$D/app.yaml" unknown "$RS"
  done
  # Round 23's F2: a case-only directory rename under the old spelling's
  # glob is added inside code_paths, not outside.
  T="$SANDBOX/r23-case-rename"; mkrepo "$T"
  printf 'a: 1\n' | wr "$T/docs/x.yaml"; printf 'r\n' | wr "$T/README"
  cm "$T" base; B_T="$(git -C "$T" rev-parse HEAD)"
  git -C "$T" mv docs Docs.tmp && git -C "$T" mv Docs.tmp Docs
  git -C "$T" commit -q -m head
  run_check "$T" --base "$B_T" --head HEAD --code-paths '["docs/**"]'
  expect_file "paths: r23 F2 (case-rename): the renamed spelling is inside the old one's glob" Docs/x.yaml executable inside_code_paths:added
fi

# ---- section: paths-glob24 -------------------------------------------------------
if mc_sec paths-glob24; then
  echo "== paths-glob24"
  # Round 24's F1-F3: a folded glob carries the glob's matching, not its
  # text. `?` and every class widen to `.*` (one code point may fold to two,
  # to none, or out of a negated class or a range); brace alternatives are
  # judged one by one; a wild literal is judged unescaped, and one with
  # wildcards by its `~`; and `/**` may stand for no directory. `ctl-plain`
  # was already right; `z-range` once could not be folded at all.
  for V in negated negated-upper range-across q-nfd class-nfd q-sharp-s q-ignorable brace-short brace-dot q-short escaped-short recursive-parent ctl-plain z-range; do
    T="$SANDBOX/r24-glob-$V"; mkrepo "$T"
    D=config; F=app.yaml; RS=inside_code_paths:no_grammar:.yaml
    case "$V" in
      negated) GL='[!c]onfig/**' ;;
      negated-upper) D=Config; GL='[!C]onfig/**' ;;
      range-across) D=docs; GL='[A-c]ocs/**' ;;
      q-nfd) D="$(printf 'cafe\314\201')"; GL='caf?/**' ;;
      class-nfd) D="$(printf 'cafe\314\201')"; GL="$(printf 'caf[\303\251\303\250]/**')" ;;
      q-sharp-s) D=strasse; GL='stra?e/**' ;;
      q-ignorable) GL='co?nfig/**' ;;
      brace-short) D=configuration; GL='{CONFIG~1,src}/**'; RS='code_paths_wild:{CONFIG~1,src}/**' ;;
      brace-dot) GL='{config.,src}/**' ;;
      q-short) D=configuration; GL='CONFIG~?/**'; RS='code_paths_wild:CONFIG~?/**' ;;
      escaped-short) D=configuration; GL='CONFIG\\~1/**'; RS='code_paths_wild:CONFIG\\~1/**' ;;
      recursive-parent) D=src; F=settings.yaml; GL='src/**/SETTIN~1.YAM'; RS='code_paths_wild:src/**/SETTIN~1.YAM' ;;
      ctl-plain) D=src; F=settings.yaml; GL='src/SETTIN~1.YAM'; RS='code_paths_wild:src/SETTIN~1.YAM' ;;
      z-range) D=zonfig; GL='[Z-a]onfig/**' ;;
    esac
    printf 'a: 1\n' | wr "$T/$D/$F"; printf 'r\n' | wr "$T/README"
    cm "$T" base; B_T="$(git -C "$T" rev-parse HEAD)"
    printf 'a: 2\n' | wr "$T/$D/$F"
    cm "$T" head
    run_check "$T" --base "$B_T" --head HEAD --code-paths "[\"$GL\"]"
    expect_file "paths: r24 F1-F3 ($V): a glob that may name the file holds it or is unknown" "$D/$F" unknown "$RS"
  done
fi

# ---- section: paths-glob25 -------------------------------------------------------
if mc_sec paths-glob25; then
  echo "== paths-glob25"
  # Round 25's F2-F5: a glob is parsed as globset parses it before anything
  # reads its components. Adjacent `**` collapse as globset collapses them
  # (`src/**/**` is `src/**`; F2), a `/` inside a class or an escape is no
  # split and a class inside braces is skipped (F2), the leading `**` forms
  # read as globset reads them (F3), and `?` and a class match a BYTE, so
  # `[!/][!/]` holds `é` (F2). A `?` or class that can supply the `~` of an
  # 8.3 name makes the component wild (F4), and a literal `~` still does
  # where no `*` could complete one (`tilde-literal`). Marks at a literal's
  # edge a wildcard touches are dropped from the folded glob (F5).
  for V in dstar-dstar mid-dstar-dstar class-noslash class-slash escaped-slash brace-class lead-dstar-dstar lead-dstar-slash lead-dd-file lead-dd-star q-short class-short q-short-file tilde-literal byte-class mark-case mark-order; do
    T="$SANDBOX/r25-glob-$V"; mkrepo "$T"
    D=Config; F=app.yaml; RS=inside_code_paths:no_grammar:.yaml; FN=F2
    case "$V" in
      dstar-dstar) D=Src; GL='src/**/**' ;;
      mid-dstar-dstar) D=A/x; F=b.yaml; GL='a/**/**/b.yaml' ;;
      class-noslash) GL='config/[!/]*' ;;
      class-slash) GL='config[/]app.yaml' ;;
      escaped-slash) GL='config\\/app.yaml' ;;
      brace-class) GL='{x[}]y,config}/**' ;;
      lead-dstar-dstar) FN=F3; D=; GL='**/**' ;;
      lead-dstar-slash) FN=F3; D=; GL='**/' ;;
      lead-dd-file) FN=F3; D=; GL='**/**/app.yaml' ;;
      lead-dd-star) FN=F3; D=; GL='**/**/*.yaml' ;;
      q-short) FN=F4; D=configuration; GL='CONFIG?1/**'; RS="code_paths_wild:$GL" ;;
      class-short) FN=F4; D=configuration; GL='CONFIG[~]1/**'; RS="code_paths_wild:$GL" ;;
      q-short-file) FN=F4; D=src; F=settings.yaml; GL='src/SETTIN?1.YAM'; RS="code_paths_wild:$GL" ;;
      tilde-literal) FN=F4; D=configuration; GL='config~x*/**'; RS="code_paths_wild:$GL" ;;
      byte-class) D="$(printf 'x\303\251')"; GL='x[!/][!/]/app.yaml' ;;
      mark-case) FN=F5; D="$(printf 'A\314\201\314\226')"; GL="$(printf 'a*\314\226/app.yaml')" ;;
      mark-order) FN=F5; D="$(printf 'a\314\226\314\201')"; GL="$(printf 'a*\314\226/app.yaml')" ;;
    esac
    P="${D:+$D/}$F"
    printf 'a: 1\n' | wr "$T/$P"; printf 'r\n' | wr "$T/README"
    cm "$T" base; B_T="$(git -C "$T" rev-parse HEAD)"
    printf 'a: 2\n' | wr "$T/$P"
    cm "$T" head
    run_check "$T" --base "$B_T" --head HEAD --code-paths "[\"$GL\"]"
    expect_file "paths: r25 $FN ($V): a glob globset matches the file by, or folds to, holds it or is unknown" "$P" unknown "$RS"
  done
fi

# ---- section: paths-glob26 -------------------------------------------------------
if mc_sec paths-glob26; then
  echo "== paths-glob26"
  # Round 26's F2: the 8.3 check runs across the whole alternative, as
  # globset matches a path. A `?`, a class holding `/`, a `*` or a `**` form
  # may supply the `/` before a short name (`S1`-`S7`). A `**/` past the
  # start may be empty, gluing what is either side into one name (`S8`,
  # `S10`), and the run after `/**` begins the next one (`S11`). `S9` is the
  # same name with no separator, the control. `rprefix-tilde` pins the
  # literal-character check's components: a later `**/` cuts none, so the
  # literal `~` is in the first component, at the root.
  # Round 26's F3: coord trims a glob as Rust's `str::trim` does, which
  # keeps U+001C-U+001F, so a glob naming one is live (`trim-1c`).
  for V in S1 S2 S3 S4 S5 S6 S7 S8 S9 S10 S11 rprefix-tilde trim-1c; do
    T="$SANDBOX/r26-glob-$V"; mkrepo "$T"
    P=src/configuration/app.yaml; FN=F2; RS=
    case "$V" in
      S1) GL='src?CONFIG?1/**' ;;
      S2) GL='src*CONFIG?1/**' ;;
      S3) GL='src[/]CONFIG[~]1/**' ;;
      S4) P=tools/configuration/run.cfg; GL='tools*CONFIG[~]1/**' ;;
      S5) P=a/configuration/app.yaml; GL='a?CONF?1/**' ;;
      S6) P=configuration/app.yaml; GL='CONFIG?1?app.yaml' ;;
      S7) P=configuration/app.yaml; GL='CONFIG?1*.yaml' ;;
      S8) P=aconf.yaml; GL='a{**/CONF?1.YAM,x}' ;;
      S9) P=aconf.yaml; GL='aCONF?1.YAM' ;;
      S10) P=aconf.yaml; GL='a{**/CONF~1.YAM,x}' ;;
      S11) P=x/configuration/app.yaml; GL='{x/**}~1/**' ;;
      rprefix-tilde) P=config/app.yaml; GL='abc{**/d~x,q}' ;;
      trim-1c) FN=F3; P="$(printf '\034').yaml"; GL='\u001c.yaml'; RS=inside_code_paths:no_grammar:.yaml ;;
    esac
    [ -n "$RS" ] || RS="code_paths_wild:$GL"
    printf 'a: 1\n' | wr "$T/$P"; printf 'r\n' | wr "$T/README"
    cm "$T" base; B_T="$(git -C "$T" rev-parse HEAD)"
    printf 'a: 2\n' | wr "$T/$P"
    cm "$T" head
    run_check "$T" --base "$B_T" --head HEAD --code-paths "[\"$GL\", \"nothing/**\"]"
    expect_file "paths: r26 $FN ($V): a glob that may name the file by another spelling holds it or is unknown" "$P" unknown "$RS"
  done
  # Round 27's F4: the automaton's hit at a `**/` past the start (`u3`: the
  # short name ends where the `**/` supplies its `/`) and its multi-byte arm
  # (`u4`: `[!/][!/]` is one two-byte character, so the base is six long).
  for V in u3 u4; do
    T="$SANDBOX/r27-glob-$V"; mkrepo "$T"
    P=configuration/x.yaml
    case "$V" in
      u3) GL='CONF?1{**/x.yaml,y}' ;;
      u4) GL='CONFI[!/][!/][~]1/**' ;;
    esac
    printf 'a: 1\n' | wr "$T/$P"; printf 'r\n' | wr "$T/README"
    cm "$T" base; B_T="$(git -C "$T" rev-parse HEAD)"
    printf 'a: 2\n' | wr "$T/$P"
    cm "$T" head
    run_check "$T" --base "$B_T" --head HEAD --code-paths "[\"$GL\", \"src/**\"]"
    expect_file "paths: r27 F4 ($V): a glob that can spell a short name makes the entry unknown" "$P" unknown "code_paths_wild:$GL"
  done
fi

# ---- section: paths-twin ---------------------------------------------------------
if mc_sec paths-twin; then
  echo "== paths-twin"
  # Round 16's F1 twin pass, where its code_paths arm still acts: a file a
  # language arm judged never consults code_paths itself (the folded glob
  # holds every other fold twin since round 25), so a twin inside them, by
  # its whole path or under a twinned directory, makes it unknown.
  if [ "$TS_OK" = 1 ]; then
    for V in whole dir; do
      T="$SANDBOX/r25-twin-lang-$V"; mkrepo "$T"
      printf '#!/bin/sh\n# note one\necho hi\n' | wr "$T/tools/run.sh"
      case "$V" in
        whole) printf '#!/bin/sh\necho x\n' | wr "$T/Tools/run.sh"; RS=fold_twin:Tools/run.sh ;;
        dir) printf '#!/bin/sh\necho x\n' | wr "$T/Tools/other.sh"; RS=fold_twin_dir:tools:Tools/run.sh ;;
      esac
      cm "$T" base; B_T="$(git -C "$T" rev-parse HEAD)"
      printf '#!/bin/sh\n# note two\necho hi\n' | wr "$T/tools/run.sh"
      cm "$T" head
      run_check "$T" --base "$B_T" --head HEAD --code-paths '["Tools/**"]'
      expect_file "paths: r16 F1 (lang-$V): a language file whose fold twin is inside code_paths is unknown" tools/run.sh unknown "$RS"
    done
  else
    ts_unmeasured "paths-twin: r16 F1 (lang-whole, lang-dir)"
  fi
  # Round 24's F6: with both a whole-path twin and an ancestor twin that
  # compose the same spelling, the whole path's reason wins.
  T="$SANDBOX/r24-twin-order"; mkrepo "$T"
  printf '#!/usr/bin/env python3\nprint(1)\n' | wr "$T/Config/tool"; printf 'notes\n' | wr "$T/config/tool"
  cm "$T" base; B_T="$(git -C "$T" rev-parse HEAD)"
  printf 'notes 2\n' | wr "$T/config/tool"
  cm "$T" head
  run_check "$T" --base "$B_T" --head HEAD --code-paths '["src/**"]'
  expect_file "paths: r24 F6: the whole-path twin's reason wins over the ancestor's" config/tool unknown fold_twin:Config/tool
  # Round 23's F3: a name that is not UTF-8 is read with lone surrogates.
  # Git for Windows writes byte 0xE9 as U+00E9 and byte 0x81 as `81`, so
  # `app<E9>.yaml` IS `appé.yaml` there and `x<81>.yaml` IS `x81.yaml`.
  for V in latin1-file latin1-dir c1-hex; do
    T="$SANDBOX/r23-undecodable-$V"; mkrepo "$T"
    case "$V" in
      latin1-file) C="docs/$(printf 'app\303\251.yaml')"; D="docs/$(printf 'app\351.yaml')"; GL="$C"; P='docs/app\udce9.yaml'; RS='undecodable:docs/app\udce9.yaml' ;;
      latin1-dir) C="$(printf 'conf\303\251')/settings.yaml"; D="$(printf 'conf\351')/app.yaml"; GL="$(printf 'conf\303\251')/**"; P='conf\udce9/app.yaml'; RS='undecodable:conf\udce9' ;;
      c1-hex) C=docs/x81.yaml; D="docs/$(printf 'x\201.yaml')"; GL="$C"; P='docs/x\udc81.yaml'; RS='undecodable:docs/x\udc81.yaml' ;;
    esac
    printf 'a: 1\n' | wr "$T/$C"; printf 'a: 1\n' | wr "$T/$D"
    cm "$T" base; B_T="$(git -C "$T" rev-parse HEAD)"
    printf 'a: 2\n' | wr "$T/$D"
    cm "$T" head
    run_check "$T" --base "$B_T" --head HEAD --code-paths "[\"$GL\"]"
    expect_file "paths: r23 F3 ($V): a name that is not UTF-8 is unknown" "$P" unknown "$RS"
  done
  # Round 21: the resolver judges an 8.3 short name too. A link to
  # `configuration/x.txt` beside a tracked `CONFIG~1/` cannot be followed
  # exactly, so it aliases every candidate.
  T="$SANDBOX/r21-link-short-name"; mkrepo "$T"
  printf 'x\n' | wr "$T/configuration/x.txt"; printf 'y\n' | wr "$T/CONFIG~1/y.txt"; printf 'one\n' | wr "$T/docs/a.md"
  ln -s configuration/x.txt "$T/l"
  cm "$T" base; B_T="$(git -C "$T" rev-parse HEAD)"
  printf 'two\n' | wr "$T/docs/a.md"
  cm "$T" head
  run_check "$T" --base "$B_T" --head HEAD --code-paths '["src/**"]'
  expect_file "paths: r21 8.3 (link): a link whose target has a short-name sibling aliases every candidate" docs/a.md unknown aliased_by_symlink:l
fi

# ---- section: paths-worktree -----------------------------------------------------
if mc_sec paths-worktree; then
  echo "== paths-worktree"
  W="$SANDBOX/worktree"; mkrepo "$W"
  printf 'one\n' | wr "$W/docs/a.md"; cm "$W" base
  printf 'two\n' | wr "$W/docs/a.md"
  printf 'u\n' | wr "$W/docs/u.md"
  printf 'x = 1\n' | wr "$W/src/u.py"
  run_check "$W" --base HEAD --code-paths '["src/**"]'
  expect_file "paths: the working tree: an uncommitted doc edit is non_executable" docs/a.md non_executable outside_code_paths
  expect_file "paths: the working tree: an untracked doc is non_executable" docs/u.md non_executable outside_code_paths
  expect_file "paths: the working tree: an untracked source file is executable" src/u.py executable added
  expect_out "paths: the working tree is named as the head" "head=WORKTREE"
  # Round 23's F1: an untracked nested repository is one `dir/` entry git
  # does not look inside. It is judged as the gitlink it would be committed
  # as (round 10's B4): unknown outside code_paths, added inside them.
  N="$SANDBOX/worktree-nested"; mkrepo "$N"
  printf 'r\n' | wr "$N/README"; cm "$N" base
  git init -q "$N/vendor/tool"; printf '#!/bin/sh\nexit 0\n' | wr "$N/vendor/tool/run.sh"; cm "$N/vendor/tool" x
  git init -q "$N/src/inner"
  printf 'u\n' | wr "$N/docs/u.md"
  run_check "$N" --base HEAD --code-paths '["src/**"]'
  expect_file "paths: r23 F1: an untracked nested repository is unknown outside code_paths" vendor/tool unknown submodule_outside_code_paths
  expect_file "paths: r23 F1: an empty nested repository inside code_paths is added code" src/inner executable inside_code_paths:added
  expect_file "paths: r23 F1: an untracked file beside them is still judged" docs/u.md non_executable outside_code_paths
  # Round 24's F4: a path the diff records as deleted whose name is on disk
  # is judged by what is on disk, as the change committed would be. A
  # nested repository with no commit replacing a tracked file (`p1`; git
  # lists it nowhere else), one after `git rm --cached` (`p5`), and a file
  # left on disk by `git rm --cached` and given a doctest (`rmcached`). A
  # plain directory replacing the file stays a deletion, and its contents
  # are judged as untracked files (`p3`).
  for V in p1 p5 rmcached p3; do
    N="$SANDBOX/worktree-r24-$V"; mkrepo "$N"
    P=vendor/tool; [ "$V" = rmcached ] && P=docs/guide.txt
    printf 'Doc.\n' | wr "$N/$P"; printf 'r\n' | wr "$N/README"; cm "$N" base
    case "$V" in
      p1) rm "$N/$P"; git init -q "$N/$P"; printf 'import os\n' | wr "$N/$P/conftest.py" ;;
      p5) git -C "$N" rm -q --cached "$P"; rm "$N/$P"; git init -q "$N/$P"; printf 'import os\n' | wr "$N/$P/conftest.py"; cm "$N/$P" x ;;
      rmcached) git -C "$N" rm -q --cached "$P"; printf 'Doc.\n\n>>> 1/0\n' | wr "$N/$P" ;;
      p3) rm "$N/$P"; printf 'import os\n' | wr "$N/$P/conftest.py" ;;
    esac
    run_check "$N" --base HEAD --code-paths '["src/**"]'
    case "$V" in
      p1|p5) expect_file "paths: r24 F4 ($V): a nested repository replacing a tracked file is a gitlink" "$P" unknown submodule_outside_code_paths ;;
      rmcached) expect_file "paths: r24 F4 (rmcached): a file left on disk is judged by what is on disk" "$P" executable doctest_marker_in_text ;;
      p3) expect_file "paths: r24 F4 (p3): a plain directory replacing a file leaves a deletion" "$P" non_executable outside_code_paths
          expect_file "paths: r24 F4 (p3): ... and its contents are judged" "$P/conftest.py" executable added ;;
    esac
  done
fi

# ---- section: paths-wt25 ---------------------------------------------------------
if mc_sec paths-wt25; then
  echo "== paths-wt25"
  # Round 25's F1: a commit of the index records the deletion, and
  # `git add -A` the file left on disk, so both are judged and the worse
  # reading stands (`i5`). A file left on disk and ignored is no replacement:
  # `git add -A` leaves it out (`i1`, `i3`; `i6`, whose copy on disk gained a
  # doctest, reads as the deletion it is). Nothing replaces a path under a
  # link, which git does not follow (`q4`, a directory turned into a link to
  # a copy of it; `q4b`, a link to a directory holding a nested repository).
  # F6: a link replacing a file is read as a link (`q5`). And a link git
  # does not track yet aliases a file as a tracked one does (`ulink`).
  for V in i1 i3 i5 i6 q4 q4b q5 ulink; do
    N="$SANDBOX/worktree-r25-$V"; mkrepo "$N"
    P=docs/guide.txt; DOC='Doc.\n\n>>> 1/0\n'
    case "$V" in
      i3) P=tools/run.sh; DOC='#!/bin/sh\necho hi\n' ;;
      i6) DOC='Doc.\n' ;;
      q4) P=vendor/tool/guide.txt ;;
      q4b) P=vendor/tool/x; DOC='notes\n' ;;
      q5) P=vendor/tool; DOC='notes\n'; printf 'import os\n' | wr "$N/lib/conftest.py" ;;
      ulink) P=docs/a.md; DOC='one\n' ;;
    esac
    printf "$DOC" | wr "$N/$P"; printf 'r\n' | wr "$N/README"; cm "$N" base
    case "$V" in
      i1|i3) git -C "$N" rm -q --cached "$P"; printf '%s\n' "$P" >>"$N/.git/info/exclude" ;;
      i5) git -C "$N" rm -q --cached "$P" ;;
      i6) git -C "$N" rm -q --cached "$P"; printf 'Doc.\n\n>>> 1/0\n' | wr "$N/$P"; printf '%s\n' "$P" >>"$N/.git/info/exclude" ;;
      q4) mkdir -p "$N/elsewhere"; cp "$N/$P" "$N/elsewhere/guide.txt"; rm -r "$N/vendor/tool"; ln -s ../elsewhere "$N/vendor/tool" ;;
      q4b) git init -q "$N/elsewhere/x"; rm -r "$N/vendor/tool"; ln -s ../elsewhere "$N/vendor/tool" ;;
      q5) git -C "$N" rm -q --cached "$P"; rm "$N/$P"; ln -s ../lib "$N/$P" ;;
      ulink) printf 'two\n' | wr "$N/$P"; ln -s docs/a.md "$N/l" ;;
    esac
    run_check "$N" --base HEAD --code-paths '["src/**"]'
    case "$V" in
      i1|i5|q4) expect_file "paths: r25 F1 ($V): a deleted doctest is judged as the deletion" "$P" executable doctest_marker_in_text:deleted:prompt ;;
      i3) expect_file "paths: r25 F1 (i3): an ignored script left on disk is still deleted" "$P" executable deleted ;;
      i6) expect_file "paths: r25 F1 (i6): an ignored file left on disk replaces nothing" "$P" non_executable outside_code_paths ;;
      q4b) expect_file "paths: r25 F1 (q4b): a nested repository through a link replaces nothing" "$P" non_executable outside_code_paths ;;
      q5) expect_file "paths: r25 F6 (q5): a link replacing a file is a link" "$P" unknown symlink_outside_code_paths ;;
      ulink) expect_file "paths: r25 (ulink): a link git does not track yet aliases a file" "$P" unknown aliased_by_symlink:l ;;
    esac
  done
fi

# ---- section: paths-wt26 ---------------------------------------------------------
if mc_sec paths-wt26; then
  echo "== paths-wt26"
  # Round 26's F1: in the working tree every head side is read whole, as
  # the commit would carry it: an untracked add (`a1`), a staged one (`a2`),
  # one staged and then edited (`a2d`), a mode change with an edit (`a3`),
  # and a doctest past the 256 bytes once read for an extensionless file
  # (`a4`). A commit of the index is judged too, where the working tree no
  # longer holds what is staged: a doctest staged and then edited out
  # (`a2i`), or staged and then removed (`a2g`). `d5`: a file `git rm
  # --cached` took out, replaced by a directory holding a doctest.
  for V in a1 a2 a2d a2i a2g a3 a4 d5; do
    N="$SANDBOX/worktree-r26-$V"; mkrepo "$N"
    P=docs/guide.txt; RS=doctest_marker_in_text:added:prompt
    printf 'r\n' | wr "$N/README"
    case "$V" in
      a3) printf 'Doc.\n' | wr "$N/$P"; RS=doctest_marker_in_text ;;
      d5) P=vendor/tool; printf 'notes\n' | wr "$N/$P" ;;
    esac
    cm "$N" base
    case "$V" in
      a1) printf 'Doc.\n\n>>> 1/0\n' | wr "$N/$P" ;;
      a2) printf 'Doc.\n\n>>> 1/0\n' | wr "$N/$P"; git -C "$N" add "$P" ;;
      a2d) printf 'Doc.\n' | wr "$N/$P"; git -C "$N" add "$P"; printf 'Doc.\n\n>>> 1/0\n' | wr "$N/$P" ;;
      a2i) printf 'Doc.\n\n>>> 1/0\n' | wr "$N/$P"; git -C "$N" add "$P"; printf 'Doc.\n' | wr "$N/$P" ;;
      a2g) printf 'Doc.\n\n>>> 1/0\n' | wr "$N/$P"; git -C "$N" add "$P"; rm "$N/$P" ;;
      a3) git -C "$N" config core.filemode true; printf 'Doc.\n\n>>> 1/0\n' | wr "$N/$P"; chmod +x "$N/$P" ;;
      a4) P=docs/NOTES; { printf 'Notes.\n'; for i in $(seq 1 40); do printf 'line %02d of prose\n' "$i"; done; printf '\n>>> 1/0\n'; } | wr "$N/$P" ;;
      d5) git -C "$N" rm -q --cached "$P"; rm "$N/$P"; P=vendor/tool/guide.txt; printf 'Doc.\n\n>>> 1/0\n' | wr "$N/$P" ;;
    esac
    run_check "$N" --base HEAD --code-paths '["src/**"]'
    expect_file "paths: r26 F1 ($V): a working-tree head is read whole, as the commit would carry it" "$P" executable "$RS"
  done
fi

# ---- section: paths-pathspec -----------------------------------------------------
if mc_sec paths-pathspec; then
  echo "== paths-pathspec"
  # Round 27's F1: a repository path is never pathspec magic. A top-level
  # name starting with `:` was read by `git diff -- <path>` as magic, which
  # named another path or none, and the head read as the base (`k1`, `k2`,
  # `k4` `:(literal)x.txt`, `k5` `:/x.txt` in a directory named `:`), and
  # an inherited GIT_ICASE_PATHSPECS, which `--literal-pathspecs` refuses, is
  # stripped with the rest of the inherited git scope rather than failing
  # every run (`k6`).
  # Round 27's F2: a file read off disk is read as git would store it. Since
  # round 28 a UTF-16LE script under `working-tree-encoding` (`wtenc`) is
  # unknown outright, as every checkout attribute is; and the base-only
  # filter (`filter-base`) is a changed `.gitattributes`.
  # Round 27's F3: a path whose `filter` attribute is set, at either side,
  # is unknown: its stored form is not the file. A smudge/clean driver in the
  # working tree (`filter-wt`), on an untracked file (`filter-new`), in a
  # commit (`filter-cm`), and only in the BASE commit's attributes
  # (`filter-base`: the base stored the doctest encoded, the head removes it
  # in plain text), which only the base tree's own attributes reveal.
  for V in k1 k2 k4 k5 k6 wtenc filter-wt filter-new filter-cm filter-base; do
    N="$SANDBOX/r27-$V"; mkrepo "$N"
    P=docs/guide.txt; HEADARG=; EV=executable; RS=doctest_marker_in_text
    printf 'r\n' | wr "$N/README"
    case "$V" in
      k1|k6) P=':notes.txt' ;;
      k2) P=':notes.txt'; RS=doctest_marker_in_text:added:prompt ;;
      k4) P=':(literal)x.txt' ;;
      k5) P=':/x.txt' ;;
      wtenc) P=tools/run; EV=unknown; RS=checkout_attribute:working-tree-encoding=UTF-16LE; printf 'tools/* working-tree-encoding=UTF-16LE\n' | wr "$N/.gitattributes" ;;
      filter-*)
        printf '#!/bin/sh\nbase64 | tr -d "\\n"; echo\n' | wr "$N/.f/enc"; printf '#!/bin/sh\nbase64 -d\n' | wr "$N/.f/dec"; chmod +x "$N/.f/enc" "$N/.f/dec"
        git -C "$N" config filter.b64.clean "$N/.f/enc"; git -C "$N" config filter.b64.smudge "$N/.f/dec"
        printf '.f/\n' >>"$N/.git/info/exclude"
        printf 'docs/*.txt filter=b64\n' | wr "$N/.gitattributes"; EV=unknown; RS=checkout_attribute:filter=b64
        [ "$V" = filter-base ] && RS=attributes_source_changed:.gitattributes ;;
    esac
    case "$V" in
      k2|wtenc|filter-new) ;;
      filter-base) printf 'Doc.\n\n>>> 1/0\n' | wr "$N/$P" ;;
      *) printf 'Doc.\n' | wr "$N/$P" ;;
    esac
    cm "$N" base; B_N="$(git -C "$N" rev-parse HEAD)"
    case "$V" in
      k2) printf 'Doc.\n' | wr "$N/$P"; git -C "$N" add -- "$N/$P"; printf 'Doc.\n\n>>> 1/0\n' | wr "$N/$P" ;;
      wtenc) printf '#!/bin/sh\nrm -rf "$HOME"\n' | iconv -f UTF-8 -t UTF-16LE | wr "$N/$P" ;;
      filter-cm) printf 'Doc.\n\n>>> 1/0\n' | wr "$N/$P"; cm "$N" head; HEADARG=HEAD ;;
      filter-base) git -C "$N" rm -q .gitattributes; printf 'Doc.\n' | wr "$N/$P"; git -C "$N" add -A; git -C "$N" commit -q -m head; HEADARG=HEAD ;;
      *) printf 'Doc.\n\n>>> 1/0\n' | wr "$N/$P" ;;
    esac
    [ "$V" = k6 ] && export GIT_ICASE_PATHSPECS=1
    if [ -n "$HEADARG" ]; then
      run_check "$N" --base "$B_N" --head HEAD --code-paths '["src/**"]'
    else
      run_check "$N" --base HEAD --code-paths '["src/**"]'
    fi
    unset GIT_ICASE_PATHSPECS
    expect_file "paths: r27 ($V): a path is read as git stores it, or is unknown where git cannot say" "$P" "$EV" "$RS"
  done
  if [ "$TS_OK" = 1 ]; then
    N="$SANDBOX/r27-k3"; mkrepo "$N"
    printf 'notes\n' | wr "$N/:run"; printf 'r\n' | wr "$N/README"; cm "$N" base
    printf '#!/bin/sh\nrm -rf "$HOME"\n' | wr "$N/:run"
    run_check "$N" --base HEAD --code-paths '["src/**"]'
    expect_file "paths: r27 F1 (k3): a top-level \`:run\` that becomes a script is read as itself" ':run' executable directive_comment
  else
    ts_unmeasured "paths-pathspec: r27 F1 (k3)"
  fi
fi

# ---- attribute rows (top level: paths-attr and paths-attrlocal run them) ---------
# Round 28: git attributes change what a checkout holds, and are
# fail-closed as a class. Each row reads non_executable or executable on
# 038c1a67's checker.
#   ident / eol: a plain edit under an unchanged `ident` (F3, P1) or
#     `text eol=crlf` attribute; iga-unset: an IGNORED working-tree
#     `.gitattributes` saying `-text`, so the sides differ though none sets it.
#   p2 / p2wt / p4: `.gitattributes` alone gains `text eol=crlf` or a
#     filter (F2); p5: the base's working-tree-encoding is dropped with the
#     edit (F1, P5).
#   ia: the index stages a filter and its encoded doctest, and the working
#     tree is put back (F4).
#   iga / iga-src / iga-cfg: a smudge-only doctest under an IGNORED
#     working-tree `.gitattributes`, read plainly, under GIT_ATTR_SOURCE
#     naming the empty tree, and under `attr.tree` naming it (F5); w1: the
#     reviewer's W1, an untracked one, under GIT_ATTR_SOURCE.
#   e4: a replace ref swaps the head blob for the base's (F6).
#   c1: both commits carry the filter, the checkout's own `.gitattributes`
#     does not (F7).
#   local-info / local-cfg: $GIT_DIR/info/attributes unsets the tracked
#     filter, or core.attributesFile adds `ident` (it ranks below a tracked
#     `.gitattributes`), on this machine only.
attr_case() { # <variant>
  local V="$1"
  N="$SANDBOX/r28-$V"; mkrepo "$N"
  P=docs/guide.txt; EV=unknown; HEADARG=HEAD; ENVSET=
  printf 'r\n' | wr "$N/README"
  printf '#!/bin/sh\nbase64 | tr -d "\\n"; echo\n' | wr "$N/.f/enc"; printf '#!/bin/sh\nbase64 -d\n' | wr "$N/.f/dec"
  printf '#!/bin/sh\nsed "s/^PROMPT/>>>/"\n' | wr "$N/.f/prompt"; chmod +x "$N/.f/enc" "$N/.f/dec" "$N/.f/prompt"
  git -C "$N" config filter.b64.clean "$N/.f/enc"; git -C "$N" config filter.b64.smudge "$N/.f/dec"
  git -C "$N" config filter.p.clean cat; git -C "$N" config filter.p.smudge "$N/.f/prompt"
  printf '.f/\n' >>"$N/.git/info/exclude"
  case "$V" in
    ident|ident-wt) printf 'docs/*.txt ident\n' | wr "$N/.gitattributes"; printf 'Doc $Id$ v1.\n' | wr "$N/$P"; RS='checkout_attribute:ident=set' ;;
    eol) printf 'docs/*.txt text eol=crlf\n' | wr "$N/.gitattributes"; printf 'Doc v1.\n' | wr "$N/$P"; RS='checkout_attribute:text=set' ;;
    p5) printf 'docs/*.txt working-tree-encoding=UTF-16LE\n' | wr "$N/.gitattributes"
        printf 'Doc v1.\n' | iconv -f UTF-8 -t UTF-16LE | wr "$N/$P"; RS=attributes_source_changed:.gitattributes ;;
    ia) printf '# attrs\n' | wr "$N/.gitattributes"; printf 'Doc.\n' | wr "$N/$P"; HEADARG=; RS=attributes_source_changed:.gitattributes ;;
    c1|local-info) printf 'docs/*.txt filter=b64\n' | wr "$N/.gitattributes"; printf 'Doc.\n' | wr "$N/$P" ;;
    *) printf 'Doc.\n' | wr "$N/$P" ;;
  esac
  cm "$N" base; B_N="$(git -C "$N" rev-parse HEAD)"
  case "$V" in
    ident) printf 'Doc $Id$ v2.\n' | wr "$N/$P"; cm "$N" head ;;
    ident-wt) printf 'Doc $Id$ v2.\n' | wr "$N/$P"; HEADARG= ;;
    eol) printf 'Doc v2.\n' | wr "$N/$P"; cm "$N" head ;;
    p2|p2wt) P=.gitattributes; RS=attributes_source_changed:.gitattributes; printf 'docs/*.txt text eol=crlf\n' | wr "$N/.gitattributes"
       if [ "$V" = p2 ]; then cm "$N" head; else HEADARG=; fi ;;
    p4) P=.gitattributes; RS=attributes_source_changed:.gitattributes; printf 'docs/*.txt filter=b64\n' | wr "$N/.gitattributes"; cm "$N" head ;;
    p5) git -C "$N" rm -q .gitattributes; printf 'Doc v2.\n' | wr "$N/$P"; cm "$N" head ;;
    ia) printf 'docs/*.txt filter=b64\n' | wr "$N/.gitattributes"; git -C "$N" add .gitattributes
        printf 'Doc.\n\n>>> 1/0\n' | wr "$N/$P"; git -C "$N" add -- "$P"
        printf '# attrs\n' | wr "$N/.gitattributes"; printf 'Doc.\n' | wr "$N/$P" ;;
    iga*) printf '.gitattributes\n' >>"$N/.git/info/exclude"; HEADARG=
        if [ "$V" = iga-unset ]; then printf 'docs/*.txt -text\n' | wr "$N/.gitattributes"; printf 'Doc v2.\n' | wr "$N/$P"; RS=checkout_attribute_differs:text
        else printf 'docs/*.txt filter=p\n' | wr "$N/.gitattributes"; printf 'Doc.\n\nPROMPT 1/0\n' | wr "$N/$P"; RS=checkout_attribute:filter=p; fi
        [ "$V" = iga-src ] && ENVSET=1
        [ "$V" = iga-cfg ] && git -C "$N" config attr.tree "$(git -C "$N" hash-object -t tree /dev/null)" ;;
    w1) printf 'docs/*.txt filter=p\n' | wr "$N/.gitattributes"; printf 'Doc.\n\nPROMPT 1/0\n' | wr "$N/$P"; HEADARG=; ENVSET=1; RS=attributes_source_changed:.gitattributes ;;
    e4) printf 'Doc.\n\n>>> 1/0\n' | wr "$N/$P"; cm "$N" head; EV=executable; RS=doctest_marker_in_text
        git -C "$N" replace "$(git -C "$N" rev-parse HEAD:$P)" "$(git -C "$N" rev-parse "$B_N:$P")" ;;
    c1|local-*) printf 'Doc.\n\n>>> 1/0\n' | wr "$N/$P"; cm "$N" head; RS=checkout_attribute:filter=b64
        case "$V" in
          c1) git -C "$N" checkout -q -b other; printf '# no filters here\n' | wr "$N/.gitattributes"; cm "$N" other ;;
          local-info) printf 'docs/*.txt -filter\n' | wr "$N/.git/info/attributes"; RS="local_attributes_source:" ;;
          local-cfg) printf 'docs/*.txt ident\n' | wr "$N/.f/attrs"; git -C "$N" config core.attributesFile "$N/.f/attrs"; RS="local_attributes_source:" ;;
        esac ;;
  esac
  [ -n "$ENVSET" ] && export GIT_ATTR_SOURCE="$(git -C "$N" hash-object -t tree /dev/null)"
  if [ "$V" = c1 ]; then
    run_check "$N" --base main~1 --head main --code-paths '["src/**"]'
  elif [ -n "$HEADARG" ]; then
    run_check "$N" --base "$B_N" --head HEAD --code-paths '["src/**"]'
  else
    run_check "$N" --base HEAD --code-paths '["src/**"]'
  fi
  unset GIT_ATTR_SOURCE
  expect_file "paths: r28 ($V): a path whose checkout git attributes change is unknown" "$P" "$EV" "$RS"
}

# ---- section: paths-attr ---------------------------------------------------------
if mc_sec paths-attr; then
  echo "== paths-attr"
  for V in ident ident-wt eol p2 p2wt p4 p5 ia; do attr_case "$V"; done
fi

# ---- section: paths-attrlocal ----------------------------------------------------
if mc_sec paths-attrlocal; then
  echo "== paths-attrlocal"
  for V in iga iga-src iga-cfg iga-unset w1 e4 c1 local-info local-cfg; do attr_case "$V"; done
fi

# ---- section: paths-attrsys ------------------------------------------------------
if mc_sec paths-attrsys; then
  echo "== paths-attrsys"
  # Round 29's F2: a `.gitattributes` by another case spelling is one file
  # where the filesystem folds case (`fold`). Round 29's F1: the system
  # attributes file is a local source. git names its path only through
  # `git var GIT_ATTR_SYSTEM` (git 2.42), so `sys-file`, `sys-fail` and
  # `sys-nosys` put a `git` on PATH that answers that one query from
  # $FAKE_ATTR_SYSTEM and hands every other call to the real git: a system
  # file whose macro unsets the tracked filter, a git that cannot name the
  # file, and GIT_ATTR_NOSYSTEM. They need a POSIX `git` shim, so they are
  # UNMEASURED under Windows. `sys-real` places a real /etc/gitattributes
  # through `unshare -rm` and an overlay mount, and is UNMEASURED where
  # unprivileged user namespaces or overlayfs are unavailable.
  REAL_GIT="$(command -v git)"; export REAL_GIT
  mkdir -p "$SANDBOX/fakegit"
  cat > "$SANDBOX/fakegit/git" <<'FAKEGIT'
#!/bin/sh
n=$#; last=""; prev=""
[ "$n" -ge 1 ] && eval "last=\${$n}"
[ "$n" -ge 2 ] && eval "prev=\${$((n - 1))}"
if [ "$prev" = var ] && [ "$last" = GIT_ATTR_SYSTEM ]; then
  [ "$FAKE_ATTR_SYSTEM" = __fail__ ] && { echo "fatal: unknown variable GIT_ATTR_SYSTEM" >&2; exit 1; }
  printf '%s\n' "$FAKE_ATTR_SYSTEM"; exit 0
fi
exec "$REAL_GIT" "$@"
FAKEGIT
  chmod +x "$SANDBOX/fakegit/git"
  SYS_SHIM=1
  case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) SYS_SHIM=0 ;; esac
  SYS_NS=0
  mkdir -p "$SANDBOX/ov/u" "$SANDBOX/ov/w"
  cat > "$SANDBOX/ns-py" <<NSPY
#!/bin/sh
exec unshare -rm sh -c 'mount -t overlay overlay -o lowerdir=/etc,upperdir=$SANDBOX/ov/u,workdir=$SANDBOX/ov/w /etc && exec "\$@"' sh "$PY" "\$@"
NSPY
  chmod +x "$SANDBOX/ns-py"
  printf 'probe\n' > "$SANDBOX/ov/u/exec-eq-probe"
  if command -v unshare >/dev/null 2>&1 && "$SANDBOX/ns-py" -c 'import sys; sys.exit(0 if open("/etc/exec-eq-probe").read() == "probe\n" else 1)' >/dev/null 2>&1; then SYS_NS=1; fi
  rm -f "$SANDBOX/ov/u/exec-eq-probe"
  for V in fold sys-file sys-fail sys-nosys sys-real; do
    case "$V" in
      sys-file|sys-fail|sys-nosys) [ "$SYS_SHIM" = 1 ] || { unmeasured "paths: r29 ($V): needs a POSIX git shim on PATH (not on Windows)"; continue; } ;;
      sys-real) [ "$SYS_NS" = 1 ] || { unmeasured "paths: r29 (sys-real): needs unprivileged user namespaces and overlayfs (unshare -rm)"; continue; } ;;
    esac
    N="$SANDBOX/r29-$V"; mkrepo "$N"
    P=docs/guide.txt; EV=unknown; SYSF="$SANDBOX/r29-$V.gitattributes"
    printf 'r\n' | wr "$N/README"
    printf '#!/bin/sh\nsed "s/^PROMPT/>>>/"\n' | wr "$N/.f/prompt"; chmod +x "$N/.f/prompt"
    git -C "$N" config filter.p.clean cat; git -C "$N" config filter.p.smudge "$N/.f/prompt"
    printf '.f/\n' >>"$N/.git/info/exclude"
    case "$V" in
      sys-file|sys-real) printf 'docs/*.txt filter=p foo\n' | wr "$N/.gitattributes" ;;
    esac
    printf 'Doc.\n' | wr "$N/$P"; cm "$N" base; B_N="$(git -C "$N" rev-parse HEAD)"
    case "$V" in
      fold) printf 'docs/*.txt filter=p\n' | wr "$N/.GitAttributes"; P=.GitAttributes; RS=attributes_source_changed:.GitAttributes ;;
      sys-nosys) EV=non_executable; RS=outside_code_paths ;;
    esac
    printf 'Doc.\n\nPROMPT 1/0\n' | wr "$N/docs/guide.txt"; cm "$N" head
    printf '[attr]foo -filter\n' > "$SYSF"
    case "$V" in
      sys-file) RS="local_attributes_source:$SYSF" ;;
      sys-fail) RS="local_attributes_source:GIT_ATTR_SYSTEM:unresolved" ;;
      sys-real) RS="local_attributes_source:"; cp "$SYSF" "$SANDBOX/ov/u/gitattributes" ;;
    esac
    case "$V" in
      fold) run_check "$N" --base "$B_N" --head HEAD --code-paths '["src/**"]' ;;
      sys-real)
        PY_SAVED="$PY"; PY="$SANDBOX/ns-py"
        ( unset GIT_ATTR_NOSYSTEM; run_check "$N" --base "$B_N" --head HEAD --code-paths '["src/**"]'
          printf '%s\n%s\n' "$RC" "$OUT" > "$SANDBOX/r29-sys-real.out" )
        PY="$PY_SAVED"; RC="$(head -1 "$SANDBOX/r29-sys-real.out")"; OUT="$(tail -n +2 "$SANDBOX/r29-sys-real.out")"
        rm -f "$SANDBOX/ov/u/gitattributes" ;;
      *)
        if [ "$V" = sys-fail ]; then FAKE="__fail__"; else FAKE="$SYSF"; fi
        ( [ "$V" = sys-nosys ] || unset GIT_ATTR_NOSYSTEM
          PATH="$SANDBOX/fakegit:$PATH" FAKE_ATTR_SYSTEM="$FAKE" run_check "$N" --base "$B_N" --head HEAD --code-paths '["src/**"]'
          printf '%s\n%s\n' "$RC" "$OUT" > "$SANDBOX/r29-$V.out" )
        RC="$(head -1 "$SANDBOX/r29-$V.out")"; OUT="$(tail -n +2 "$SANDBOX/r29-$V.out")" ;;
    esac
    expect_file "paths: r29 ($V): an attributes source by any spelling, or no diff, is read" "$P" "$EV" "$RS"
  done
  # Round 31: a local attribute file is read as git reads it. `vt`: an
  # info/attributes line `\v#x.txt !filter` is live to git (its parser skips
  # only ` \t\r\n` before a `#`), and unsets a tracked `* filter=lfs` for a
  # changed `\v#x.txt`. `cfg-space`: a core.attributesFile whose path ends
  # in a space defines a macro that unsets a tracked filter; a stripped path
  # names no file. `local-comment`: an info/attributes holding only blank
  # and comment lines is no source.
  for V in vt cfg-space local-comment; do
    N="$SANDBOX/r31-$V"; mkrepo "$N"
    P=docs/guide.txt; EV=unknown; RS="local_attributes_source:"
    printf 'r\n' | wr "$N/README"
    printf '#!/bin/sh\nsed "s/^PROMPT/>>>/"\n' | wr "$N/.f/prompt"; chmod +x "$N/.f/prompt"
    git -C "$N" config filter.p.clean cat; git -C "$N" config filter.p.smudge "$N/.f/prompt"
    printf '.f/\n' >>"$N/.git/info/exclude"
    case "$V" in
      vt) P=$'\v#x.txt'; printf '* filter=lfs\n' | wr "$N/.gitattributes" ;;
      cfg-space) printf 'docs/*.txt filter=p foo\n' | wr "$N/.gitattributes" ;;
      local-comment) EV=non_executable; RS=outside_code_paths ;;
    esac
    printf 'Doc.\n' | wr "$N/$P"; cm "$N" base; B_N="$(git -C "$N" rev-parse HEAD)"
    case "$V" in
      vt) printf '\v#x.txt !filter\n' | wr "$N/.git/info/attributes"; printf 'Doc v2.\n' | wr "$N/$P" ;;
      cfg-space) printf '[attr]foo -filter\n' | wr "$N/.f/attrs "; git -C "$N" config core.attributesFile "$N/.f/attrs "
         printf 'Doc.\n\nPROMPT 1/0\n' | wr "$N/$P" ;;
      local-comment) printf '# a note\n\n \t# indented\r\n' | wr "$N/.git/info/attributes"; printf 'Doc v2.\n' | wr "$N/$P" ;;
    esac
    cm "$N" head
    run_check "$N" --base "$B_N" --head HEAD --code-paths '["src/**"]'
    expect_file "paths: r31 ($V): a local attribute file is read as git reads it" "$P" "$EV" "$RS"
  done
fi

# ---- section: paths-root ---------------------------------------------------------
if mc_sec paths-root; then
  echo "== paths-root"
  # Round 32: the repo root git prints is read less only its LF. A work tree
  # whose name ends in a space, beside a clean clone without the space: a
  # stripped root would judge the clone and see no change at all.
  N="$SANDBOX/r32-root "; mkrepo "$N"; printf 'r\n' | wr "$N/README"; printf 'Doc.\n' | wr "$N/docs/g.txt"; cm "$N" base
  rm -rf "$SANDBOX/r32-root"; cp -a "$N" "$SANDBOX/r32-root"
  printf 'Doc.\n\n>>> 1/0\n' | wr "$N/docs/g.txt"
  run_check "$N" --base HEAD --code-paths '["src/**"]'
  expect_file "paths: r32 (root-space): a work tree ending in a space is judged, not its sibling" docs/g.txt executable doctest_marker_in_text
fi

# ---- section: globs ------------------------------------------------------------
if mc_sec globs; then
  echo "== globs"
  G="$SANDBOX/globs"; mkrepo "$G"
  for f in crates/x.md notes.md sub/notes.md a/b.md a/x/y/b.md docs/keep.md docs/sub/deep.md; do
    printf 'base\n' | wr "$G/$f"
  done
  cm "$G" base; BASE_G="$(git -C "$G" rev-parse HEAD)"
  for f in crates/x.md notes.md sub/notes.md a/b.md a/x/y/b.md docs/keep.md docs/sub/deep.md; do
    printf 'head\n' | wr "$G/$f"
  done
  cm "$G" head
  run_check "$G" --base "$BASE_G" --head HEAD --code-paths '["crates", "**/notes.md", "a/**/b.md"]'
  expect_file "globs: a bare directory name matches nothing (globset)" crates/x.md non_executable outside_code_paths
  expect_file "globs: a leading **/ matches at the root" notes.md executable inside_code_paths
  expect_file "globs: a leading **/ matches below the root" sub/notes.md executable inside_code_paths
  expect_file "globs: /**/ matches zero directories" a/b.md executable inside_code_paths
  expect_file "globs: /**/ matches several directories" a/x/y/b.md executable inside_code_paths
  expect_file "globs: a file no glob names is outside" docs/keep.md non_executable outside_code_paths
  run_check "$G" --base "$BASE_G" --head HEAD --code-paths '["docs/*", "{crates,zzz}/**", "[ab]/b.md"]'
  expect_file "globs: * crosses / (literal_separator is off)" docs/sub/deep.md executable inside_code_paths
  expect_file "globs: an alternate group matches" crates/x.md executable inside_code_paths
  expect_file "globs: a character class matches" a/b.md executable inside_code_paths
  expect_file "globs: a class does not widen past its members" notes.md non_executable outside_code_paths
fi

# ---- include helpers (top level: every include-* section may use them) ---------
# An OPAQUE include -- one whose path is not a plain literal, or a name that
# reaches a macro by another road -- may name any file, so every non-code
# candidate in the repository is unknown. Each shape gets its own repository
# because one opaque occurrence decides the whole repository.
OPAQUE_N=0
opaque_case() { # <name> <reason prefix> <lib.rs body (printf format)> [<src/README.md body>]
  OPAQUE_N=$((OPAQUE_N + 1))
  local O="$SANDBOX/opaque-$OPAQUE_N"; mkrepo "$O"
  printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$O/Cargo.toml"
  printf "$3" | wr "$O/src/lib.rs"
  [ -n "${4:-}" ] && printf "$4" | wr "$O/src/README.md"
  printf 'a\n' | wr "$O/src/data.txt"; printf 'a\n' | wr "$O/docs/other.md"
  cm "$O" base; local B; B="$(git -C "$O" rev-parse HEAD)"
  printf 'b\n' | wr "$O/src/data.txt"; printf 'b\n' | wr "$O/docs/other.md"
  cm "$O" head
  run_check "$O" --base "$B" --head HEAD --code-paths '["src/**/*.rs"]'
  expect_file "include: $1 makes an included file unknown" src/data.txt unknown "$2"
  expect_file "include: $1 makes every non-code file unknown" docs/other.md unknown "$2"
}
# The round-7 fixtures: a concat!-assembled doctest that includes data.txt, and
# a README holding the same doctest plainly. include-r7 and include-r9 use them.
R7_DOCTEST='concat!("```\\nlet s = inc", "lude_str!(\\"data.txt\\");\\nassert_eq!(s, \\"a\\\\n\\");\\n```")'
R7_README='Readme.\n\n```\nlet s = include_str!("data.txt");\nassert_eq!(s, "a\\n");\n```\n'

# ---- section: include-doc --------------------------------------------------------
if mc_sec include-doc; then
  echo "== include-doc"
  I="$SANDBOX/rinclude"; mkrepo "$I"
  printf '[package]\nname = "w"\nversion = "0.1.0"\n' | wr "$I/Cargo.toml"
  wr "$I/src/lib.rs" <<'EOF'
#![doc = include_str!("../README.md")]
pub mod q;
#[doc = include_str!("../docs/guide.md")]
pub fn g() {}
/// ```
pub mod sub;
EOF
  printf 'pub const Q: &str = include_str!("data.sql");\npub const Q2: &str = ::core::include_str!("q2.txt");\npub struct Cfg { pub include: u8 }\npub fn n(c: &Cfg) -> u8 { c.include }\n' | wr "$I/src/q.rs"
  printf 'q\n' | wr "$I/src/q2.txt"
  printf '# W\n\nProse.\n\n```\nassert!(true);\n```\n' | wr "$I/README.md"
  printf 'Guide.\n\n```\nlet a = 1;\n```\n' | wr "$I/docs/guide.md"
  printf 'select 1;\n' | wr "$I/src/data.sql"
  printf 'other\n' | wr "$I/docs/other.md"
  printf 'Foo\n    bar baz\n' | wr "$I/docs/ctx.md"
  printf '#[doc = include_str!("../docs/ctx.md")]\npub fn h() {}\n#[doc = include_str!("../docs/html.md")]\npub fn i() {}\n#[doc = include_str!("../docs/cmt.md")]\npub fn j() {}\n#[doc = include_str!("../docs/tick.md")]\npub fn k() {}\n' | wr "$I/src/ctx.rs"
  printf '#[doc = include_str!("../docs/nb.md")]\npub fn l() {}\n#[doc = include_str!("../docs/fs.md")]\npub fn m() {}\n#[doc = include_str!("../docs/cr.md")]\npub fn n() {}\n#[doc = include_str!("../docs/fn.md")]\npub fn o() {}\n' | wr "$I/src/ws.rs"
  printf '<div>\n```\nlet a = 1;\n```\n</div>\n' | wr "$I/docs/html.md"
  printf '<!--\n```\nlet a = 1;\n```\n-->\n' | wr "$I/docs/cmt.md"
  printf '```rust`x\nfoo\n```\nlet a = 1;\n```\n' | wr "$I/docs/tick.md"
  # A closer followed by NBSP (\302\240) or \034 is a CODE line to CommonMark,
  # which allows only spaces and tabs after a closing fence.
  printf '```\n```\302\240\nlet a = 1;\n```\n' | wr "$I/docs/nb.md"
  printf '```\n```\034\nlet a = 1;\n```\n' | wr "$I/docs/fs.md"
  # A lone CR is a line ending to CommonMark but not to Rust's str::lines;
  # which lines exist is in doubt, so the proof is refused.
  printf 'Note:\r```\rlet a = 1;\r```\n' | wr "$I/docs/cr.md"
  printf '[^n]: Note.\n  ```\n  let a = 1;\n  ```\n' | wr "$I/docs/fn.md"
  # rustdoc JOINS an included file with the item's other doc fragments: the
  # fence is opened in a `///` beside the include (join.md), or in the outer
  # docs on the `mod sub;` that declares the including file (sub.md).
  printf '/// ```\n#[doc = include_str!("../docs/join.md")]\n/// ```\npub fn p() {}\n' | wr "$I/src/join.rs"
  printf 'let a = 1;\n' | wr "$I/docs/join.md"
  printf '#![doc = include_str!("../docs/sub.md")]\npub fn s() {}\n' | wr "$I/src/sub.rs"
  printf 'let a = 1;\n```\n' | wr "$I/docs/sub.md"
  # A `///` on ANOTHER item is not a fragment of this one.
  printf '/// Unrelated.\npub fn a() {}\n\n#[doc = include_str!("../docs/solo.md")]\npub fn b() {}\n' | wr "$I/src/solo.rs"
  printf 'Solo prose.\n' | wr "$I/docs/solo.md"
  # U+200E (\342\200\216) is Rust whitespace between two tokens.
  printf 'pub const L: &str = include_str!\342\200\216("lrm.txt");\n' | wr "$I/src/lrm.rs"
  printf 'a\n' | wr "$I/src/lrm.txt"
  cm "$I" base; BASE_I="$(git -C "$I" rev-parse HEAD)"
  printf '# W\n\nProse, reworded.\n\n```\nassert!(true);\n```\n' | wr "$I/README.md"
  printf 'Guide.\n\n```\nlet a = 2;\n```\n' | wr "$I/docs/guide.md"
  printf 'select 2;\n' | wr "$I/src/data.sql"
  printf 'q2\n' | wr "$I/src/q2.txt"
  printf 'changed\n' | wr "$I/docs/other.md"
  printf 'Foo\n\n    bar baz\n' | wr "$I/docs/ctx.md"
  printf '<div>\n\n```\nlet a = 1;\n```\n</div>\n' | wr "$I/docs/html.md"
  printf 'Note:\n```\nlet a = 1;\n```\n' | wr "$I/docs/cmt.md"
  printf '```rust`x\nfoo\n```\nlet a = 2;\n```\n' | wr "$I/docs/tick.md"
  printf '```\n```\302\240\nlet a = 2;\n```\n' | wr "$I/docs/nb.md"
  printf '```\n```\034\nlet a = 2;\n```\n' | wr "$I/docs/fs.md"
  printf 'Note:\r```\rlet a = 2;\r```\n' | wr "$I/docs/cr.md"
  printf 'Note.\n  ```\n  let a = 1;\n  ```\n' | wr "$I/docs/fn.md"
  printf 'let a = 2;\n' | wr "$I/docs/join.md"
  printf 'let a = 2;\n```\n' | wr "$I/docs/sub.md"
  printf 'Solo prose, reworded.\n' | wr "$I/docs/solo.md"
  printf 'b\n' | wr "$I/src/lrm.txt"
  cm "$I" head
  run_check "$I" --base "$BASE_I" --head HEAD --code-paths '["src/**/*.rs"]'
  expect_file "include: prose in a doc-included README stays non_executable" README.md non_executable outside_code_paths:rust_doc_include_prose_only
  expect_file "include: a code block in a doc-included file is a doctest" docs/guide.md executable rust_doc_include_code_block:src/lib.rs
  expect_file "include: a non-code file include_str!'d as data is executable" src/data.sql executable included_by_rust:src/q.rs
  expect_file "include: a file no crate includes stays non_executable" docs/other.md non_executable outside_code_paths
  expect_file "include: a ::core:: qualified include of a literal is plain" src/q2.txt executable included_by_rust:src/q.rs
  expect_file "include: a blank line that turns an indented paragraph line into a doctest is executable" docs/ctx.md executable rust_doc_include_block_context:src/ctx.rs
  # A CommonMark HTML block holds the fence as raw HTML until its end
  # condition; every code line is unchanged on both sides.
  expect_file "include: a blank line that ends an HTML block before a fence is executable" docs/html.md executable rust_doc_include_block_context:src/ctx.rs
  expect_file "include: un-commenting an HTML-commented fence is executable" docs/cmt.md executable rust_doc_include_block_context:src/ctx.rs
  expect_file "include: a backtick-fence line with a backtick in its info string is not an opener" docs/tick.md executable rust_doc_include_code_block:src/ctx.rs
  # Python's \s matches NBSP and \x1c-\x1f; a fence closer followed by one is
  # code, so the edit after it is inside the doctest.
  expect_file "include: a fence closer followed by NBSP is code, not a closer" docs/nb.md executable rust_doc_include_code_block:src/ws.rs
  expect_file "include: a fence closer followed by \\x1c is code, not a closer" docs/fs.md executable rust_doc_include_code_block:src/ws.rs
  expect_file "include: a doc holding a lone CR is refused as block context" docs/cr.md executable rust_doc_include_block_context:src/ws.rs
  # Every fence line is unchanged; only the footnote label around it goes.
  expect_file "include: dropping a footnote label around a fence is executable" docs/fn.md executable rust_doc_include_block_context:src/ws.rs
  # Judged alone each file holds no doctest; joined, `let a = ...;` is one.
  expect_file "include: a file joined with a /// fragment of its item is executable" docs/join.md executable rust_doc_include_joined:src/join.rs:adjacent_doc_fragment
  expect_file "include: an inner include joined with the parent's docs on mod sub is executable" docs/sub.md executable rust_doc_include_joined:src/sub.rs:parent_outer_doc:src/lib.rs
  expect_file "include: a /// on another item does not join a doc include" docs/solo.md non_executable outside_code_paths:rust_doc_include_prose_only
  expect_file "include: an include whose tokens are separated by U+200E is still read" src/lrm.txt executable included_by_rust:src/lrm.rs
fi

# ---- section: include-opaque -----------------------------------------------------
if mc_sec include-opaque; then
  echo "== include-opaque"
  opaque_case "a use-renamed include (use core::include_str as load)" included_by_rust_opaque:src/lib.rs \
    'use core::include_str as load;\npub const X: &str = load!("data.txt");\n'
  opaque_case "a concat! argument" included_by_rust_opaque:src/lib.rs \
    'pub const X: &str = include_str!(concat!("da", "ta.txt"));\n'
  opaque_case "an env!(CARGO_MANIFEST_DIR) argument" included_by_rust_opaque:src/lib.rs \
    'pub const X: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/da", "ta.txt"));\n'
  opaque_case "a plain include inside a macro_rules body" included_by_rust_opaque:src/lib.rs \
    'macro_rules! m { () => { include_str!("data.txt") }; }\npub const X: &str = m!();\n'
  opaque_case "an include name handed to a macro" included_by_rust_opaque:src/lib.rs \
    'macro_rules! call { ($m:ident) => { $m!("data.txt") }; }\npub const X: &str = call!(include_str);\n'
  opaque_case "a doctest that includes a file" included_by_rust_opaque:src/lib.rs \
    '/// ```\n/// let _ = include_str!("data.txt");\n/// ```\npub fn f() {}\n'
  opaque_case "a literal that names no tracked file (case-folded)" included_by_rust_opaque:src/lib.rs:unresolved_path \
    'pub const X: &str = include_str!("Data.txt");\n'
  opaque_case "a #[path] module read from a non-.rs file" included_by_rust_opaque:src/lib.rs:path_attribute \
    '#[cfg_attr(unix, path = "gen.inc")]\nmod gen;\n'
  # Round-6 finding 2: rustdoc compiles a doctest out of a `#[doc = ".."]`
  # STRING, so the include inside it is read from raw text, not code tokens.
  opaque_case "a doctest include inside a #[doc = \"..\"] string" included_by_rust_opaque:src/lib.rs:raw_text:include_str \
    '#[doc = "```\nlet _ = include_str!(\\"data.txt\\");\n```"]\npub fn f() {}\n'
  # The doc text is the DECODED literal: `\x5f` is the underscore.
  opaque_case "a doctest include spelled with a string escape" included_by_rust_opaque:src/lib.rs:raw_text:escaped_name \
    '#[doc = "```\nlet _ = include\\x5fstr!(\\"data.txt\\");\n```"]\npub fn f() {}\n'
  # concat! may assemble the include out of pieces no name search sees.
  opaque_case "a doc attribute whose value is a concat!" included_by_rust_opaque:src/lib.rs:raw_text:doc_attribute_non_literal \
    '#[doc = concat!("```\\n", "let _ = inc", "lude_str!(\\"data.txt\\");\\n```")]\npub fn f() {}\n'
fi

# ---- section: include-r7 ---------------------------------------------------------
if mc_sec include-r7; then
  echo "== include-r7"
  # Round-7 findings. b1: a comment between `doc` and `=` hid the concat!
  # from the anchored raw regex.
  opaque_case "b1: a doc attribute with a comment before its =" included_by_rust_opaque:src/lib.rs:raw_text:doc_attribute_non_literal \
    "#[doc /* c */ = $R7_DOCTEST]\npub fn f() {}\n"
  # b2: a macro_rules! body building its attribute from a metavariable,
  # handed `doc = concat!(..)` after a `;`.
  opaque_case "b2: a macro body #[\$(\$m)*] handed doc = concat!" included_by_rust_opaque:src/lib.rs:macro_attribute_metavariable \
    "macro_rules! documented {\n    (\$i:ident; \$(\$m:tt)*) => { #[\$(\$m)*] pub fn \$i() {} };\n}\ndocumented!(f; doc = $R7_DOCTEST);\n"
  # b3 / b4: a plain include_str! handed to a macro that makes it doc text;
  # the README it names holds the doctest.
  opaque_case "b3: a macro body #[doc = \$s] handed include_str!(README)" included_by_rust_opaque:src/lib.rs:include_in_macro_arguments:include_str \
    'macro_rules! documented {\n    ($s:expr) => { #[doc /**/ = $s] pub fn f() {} };\n}\ndocumented!(include_str!("README.md"));\n' "$R7_README"
  opaque_case "b4: a macro handed doc = include_str!(README)" included_by_rust_opaque:src/lib.rs:include_in_macro_arguments:include_str \
    'macro_rules! documented {\n    ($i:ident, $($m:tt)*) => { #[$($m)*] pub fn $i() {} };\n}\ndocumented!(f, doc = include_str!("README.md"));\n' "$R7_README"
  # Rule 1a alone: a NESTED comment before `=` defeats any regex; the token
  # stream has no comments at all.
  opaque_case "a doc attribute with a nested comment before its =" included_by_rust_opaque:src/lib.rs:token:doc_attribute_non_literal \
    "#[doc /* /* */ */ = $R7_DOCTEST]\npub fn f() {}\n"
  # Rule 1b alone: the attribute's name and value arrive as two
  # metavariables, so no `doc =` is spelled anywhere.
  opaque_case "a macro body #[\$a = \$b] handed doc and a concat!" included_by_rust_opaque:src/lib.rs:macro_attribute_metavariable \
    "macro_rules! documented {\n    (\$a:ident, \$b:expr) => { #[\$a = \$b] pub fn f() {} };\n}\ndocumented!(doc, $R7_DOCTEST);\n"
  # Rule 2 alone: a plain include_str! target is raw-scanned wherever the
  # include sits, not only inside an attribute.
  opaque_case "a doctest include inside a const-included README" included_by_rust_opaque:src/lib.rs:included_text:src/README.md:include_str \
    'pub const R: &str = include_str!("README.md");\n' "$R7_README"
  # Round-8 rm1: an attribute ASSEMBLED in expansion. `#` reaches the macro
  # as a lone tt and is put in front of a bracket group built from
  # metavariables; no `#[`, no `doc =` is spelled anywhere. Each rule that
  # closes the class gets its own case.
  opaque_case "r8 rm1: #, doc and a concat! handed to a macro that builds the attribute" included_by_rust_opaque:src/lib.rs \
    "macro_rules! wrap { (\$h:tt \$t:tt) => { \$h \$t pub fn g() {} }; }\nmacro_rules! mk { (\$h:tt, \$d:ident, \$s:expr) => { wrap!(\$h [\$d = \$s]); }; }\nmk!(#, doc, $R7_DOCTEST);\n"
  opaque_case "r8 1c: a # that opens no attribute (a tt for a macro)" included_by_rust_opaque:src/lib.rs:stray_pound_token \
    'macro_rules! put { ($h:tt) => {}; }\nput!(#);\n'
  opaque_case "r8 1d: two metavariables side by side in a macro body" included_by_rust_opaque:src/lib.rs:macro_adjacent_metavariables \
    'macro_rules! wrap { ($h:tt $t:tt) => { $h $t pub fn g() {} }; }\n'
  opaque_case "r8 1b: a bracket group after a metavariable holding a metavariable" included_by_rust_opaque:src/lib.rs:macro_attribute_metavariable \
    'macro_rules! mk { ($h:tt, $s:expr) => { other!($h [doc = $s]); }; }\n'
  # Round-8 rp1: a doctest's `#[path = ..] mod` compiles ANY file as Rust.
  opaque_case "r8 rp1: a #[path] module inside a doctest" included_by_rust_opaque:src/lib.rs:raw_text:path_attribute \
    '/// ```\n/// #[path = "data.txt"] mod d;\n/// assert_eq!(d::X, 1);\n/// ```\npub fn f() {}\n'
fi

# ---- section: include-r9 ---------------------------------------------------------
if mc_sec include-r9; then
  echo "== include-r9"
  # Round-9 F1: an INNER attribute assembled in expansion. `#[]` reaches the
  # macro as `$p:tt []` (so the `#` is not stray) and `$p ! $q` puts a `!`
  # between the two metavariables, building `#![doc = ..]`.
  opaque_case "r9 F1: \$p ! \$q in a macro body assembles an inner attribute" included_by_rust_opaque:src/lib.rs:macro_adjacent_metavariables \
    "macro_rules! inner { (\$p:tt, \$q:tt) => { \$p ! \$q }; }\nmacro_rules! mk { (\$p:tt [], \$d:ident, \$s:expr) => { inner!(\$p, [\$d = \$s]); }; }\nmk!(#[], doc, $R7_DOCTEST);\n"
  # Round-9 F2 / F3: doctest text is CODE. A macro inside a doctest builds
  # `#[path = ..]` or an include from a name handed in as a plain word; no
  # raw name pattern sees either, the token scan run over the doctest does.
  opaque_case "r9 F2: a macro-built #[path] inside a doctest" included_by_rust_opaque:src/lib.rs:raw_text:doctest: \
    '/// ```\n/// macro_rules! m { ($a:ident) => { #[$a = "data.txt"] mod d; } }\n/// m!(path);\n/// ```\npub fn f() {}\n'
  opaque_case "r9 F3: an include name handed to a macro as a word inside a doctest" included_by_rust_opaque:src/lib.rs:raw_text:doctest: \
    '/// ```rust,no_run\n/// macro_rules! m { ($i:ident) => { $i!("data.txt") } }\n/// let _ = m!(include);\n/// ```\npub fn f() {}\n'
  opaque_case "r9 F3: the same doctest behind a hidden line in a #[doc] string" included_by_rust_opaque:src/lib.rs:raw_text:doctest: \
    '#[doc = "```\n# macro_rules! m { ($i:ident) => { $i!(\\"data.txt\\") } }\nlet _ = m!(include);\n```"]\npub fn f() {}\n'
  opaque_case "r9 F3: the same doctest in a string handed to an external macro" included_by_rust_opaque:src/lib.rs:raw_text:doctest: \
    'other_crate::documented!("```\nmacro_rules! m { ($i:ident) => { $i!(\\"data.txt\\") } }\nlet _ = m!(include);\n```");\n'
  opaque_case "r9 F3: the same doctest in a README a #![doc] includes" included_by_rust_opaque:src/lib.rs:attribute_included_text:src/README.md:doctest: \
    '#![doc = include_str!("README.md")]\npub fn f() {}\n' \
    'Readme.\n\n```\nmacro_rules! m { ($i:ident) => { $i!("data.txt") } }\nlet _ = m!(include);\n```\n'
  opaque_case "r9 F2: the same doctest inside a > quote" included_by_rust_opaque:src/lib.rs:raw_text:doctest: \
    '/// > ```\n/// > macro_rules! m { ($a:ident) => { #[$a = "data.txt"] mod d; } }\n/// > m!(path);\n/// > ```\npub fn f() {}\n'
  # ... and the doctest shapes the token scan must leave alone: hidden lines,
  # a binding named like an include, and a non-Rust fenced block.
  O="$SANDBOX/r9-neg"; mkrepo "$O"
  printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$O/Cargo.toml"
  printf '/// # Examples\n///\n/// ```\n/// # fn main() {\n/// let include = 1;\n/// ##[allow(unused)]\n/// let x = 2;\n/// # }\n/// ```\n///\n/// ```sh\n/// echo hi\n/// #comment, a stray # to Rust\n/// ```\npub fn f() {}\npub const X: &str = include_str!("data.txt");\n' | wr "$O/src/lib.rs"
  printf 'a\n' | wr "$O/src/data.txt"; printf 'a\n' | wr "$O/docs/other.md"
  cm "$O" base; B_O="$(git -C "$O" rev-parse HEAD)"
  printf 'b\n' | wr "$O/src/data.txt"; printf 'b\n' | wr "$O/docs/other.md"
  cm "$O" head
  run_check "$O" --base "$B_O" --head HEAD --code-paths '["src/**/*.rs"]'
  expect_file "include: r9 negatives: a plain include beside a scanned doctest still reads executable" src/data.txt executable included_by_rust:src/lib.rs
  expect_file "include: r9 negatives: hidden lines, an include-named binding, a sh block are not opaque" docs/other.md non_executable outside_code_paths
fi

# ---- section: include-r10 --------------------------------------------------------
if mc_sec include-r10; then
  echo "== include-r10"
  # Round 10's B1: rustdoc keeps a block Rust when a Rust attribute comes
  # BEFORE the first unknown word (`no_run,sh`, `should_panic json`), and a
  # `{..}` group is a custom attribute this split cannot read.
  opaque_case "r10 B1: a no_run,sh fence is still a doctest" included_by_rust_opaque:src/lib.rs:raw_text:doctest: \
    '/// ```no_run,sh\n/// macro_rules! m { ($i:ident) => { $i!("data.txt") } }\n/// const _: () = assert!(m!(include) == 2);\n/// ```\npub fn f() {}\n'
  opaque_case "r10 B1: a should_panic json fence is still a doctest" included_by_rust_opaque:src/lib.rs:raw_text:doctest: \
    '/// ```should_panic json\n/// macro_rules! m { ($a:ident) => { #[$a = "data.txt"] mod d; } }\n/// m!(path);\n/// ```\npub fn f() {}\n'
  opaque_case "r10 B1: a fence with a {..} group is scanned" included_by_rust_opaque:src/lib.rs:raw_text:doctest: \
    '/// ```sh{.x}\n/// macro_rules! m { ($i:ident) => { $i!("data.txt") } }\n/// let _ = m!(include);\n/// ```\npub fn f() {}\n'
  # Round 10's B3: a std macro name is trusted only while nothing rebinds it.
  opaque_case "r10 B3: a use-as rebinding of a trusted macro name" included_by_rust_opaque:src/lib.rs:raw_text:doctest: \
    'use other_crate::documented as assert_eq;\nassert_eq!("```\nmacro_rules! m { ($i:ident) => { $i!(\\"data.txt\\") } }\nlet _ = m!(include);\n```");\npub fn f() {}\n'
  opaque_case "r10 B3: a glob use that may bring a same-named macro" included_by_rust_opaque:src/lib.rs:raw_text:doctest: \
    'use other_crate::*;\nassert_eq!("```\nmacro_rules! m { ($i:ident) => { $i!(\\"data.txt\\") } }\nlet _ = m!(include);\n```");\npub fn f() {}\n'
  # ... and through another file: `#[macro_use] mod macros;` in lib.rs puts
  # macros.rs's assert_eq! in textual scope for x.rs, which has no `use`.
  X="$SANDBOX/r10-macro-use"; mkrepo "$X"
  printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$X/Cargo.toml"
  printf '#[macro_use]\nmod macros;\nmod x;\n' | wr "$X/src/lib.rs"
  printf 'macro_rules! documented { ($s:expr) => {}; }\n' | wr "$X/src/macros.rs"
  printf 'assert_eq!("```\nmacro_rules! m { ($i:ident) => { $i!(\\"data.txt\\") } }\nlet _ = m!(include);\n```");\npub fn f() {}\n' | wr "$X/src/x.rs"
  printf 'a\n' | wr "$X/src/data.txt"
  cm "$X" base; B_X="$(git -C "$X" rev-parse HEAD)"
  printf 'b\n' | wr "$X/src/data.txt"
  cm "$X" head
  run_check "$X" --base "$B_X" --head HEAD --code-paths '["src/**/*.rs"]'
  expect_file "include: r10 B3: a #[macro_use] in another file revokes the trusted name" src/data.txt unknown included_by_rust_opaque:src/x.rs:raw_text:doctest:
  # The order rule's other half: a Rust attribute AFTER a language tag does
  # not make the block Rust, so a shell comment there is not a stray #.
  O="$SANDBOX/r10-neg"; mkrepo "$O"
  printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$O/Cargo.toml"
  printf '/// ```sh,no_run\n/// #comment, a stray # to Rust\n/// ```\npub fn f() {}\npub const X: &str = include_str!("data.txt");\n' | wr "$O/src/lib.rs"
  printf 'a\n' | wr "$O/src/data.txt"; printf 'a\n' | wr "$O/docs/other.md"
  cm "$O" base; B_O="$(git -C "$O" rev-parse HEAD)"
  printf 'b\n' | wr "$O/src/data.txt"; printf 'b\n' | wr "$O/docs/other.md"
  cm "$O" head
  run_check "$O" --base "$B_O" --head HEAD --code-paths '["src/**/*.rs"]'
  expect_file "include: r10 negative: a sh,no_run block is not scanned as Rust" docs/other.md non_executable outside_code_paths
fi

# ---- section: include-r11 --------------------------------------------------------
if mc_sec include-r11; then
  echo "== include-r11"
  # Round 11's F1: rustdoc's info-string grammar has (..) comments, quoted
  # tokens, backslash escapes and entity references; any character outside a
  # plain word list makes the block Rust (scanned), never a skipped non-Rust.
  opaque_case "r11 F1: a fence whose info starts with a (..) comment" included_by_rust_opaque:src/lib.rs:raw_text:doctest: \
    '/// ```(sh) no_run\n/// macro_rules! m { ($i:ident) => { $i!("data.txt") } }\n/// let _ = m!(include);\n/// ```\npub fn f() {}\n'
  opaque_case "r11 F1: a fence with a quoted rust token" included_by_rust_opaque:src/lib.rs:raw_text:doctest: \
    '/// ```"no_run" sh\n/// macro_rules! m { ($i:ident) => { $i!("data.txt") } }\n/// let _ = m!(include);\n/// ```\npub fn f() {}\n'
  opaque_case "r11 F1: a fence with a backslash-escaped comma" included_by_rust_opaque:src/lib.rs:raw_text:doctest: \
    '/// ```no_run\\,sh\n/// macro_rules! m { ($i:ident) => { $i!("data.txt") } }\n/// let _ = m!(include);\n/// ```\npub fn f() {}\n'
  opaque_case "r11 F1: a fence spelling rust with an entity reference" included_by_rust_opaque:src/lib.rs:raw_text:doctest: \
    '/// ```&#114;ust\n/// macro_rules! m { ($i:ident) => { $i!("data.txt") } }\n/// let _ = m!(include);\n/// ```\npub fn f() {}\n'
  # Round 11's F3: `macro assert_eq(..)` (decl_macro) shadows the prelude
  # macro. Inside a doctest no tree scan runs, so the in-text rule alone
  # must see it.
  opaque_case "r11 F3: a decl_macro rebinding a trusted name inside a doctest" included_by_rust_opaque:src/lib.rs:raw_text:doctest: \
    '/// ```\n/// macro assert_eq($s:expr) { other::documented!($s) }\n/// assert_eq!("```\\nmacro_rules! m { ($i:ident) => { $i!(\\"data.txt\\") } }\\nlet _ = m!(include);\\n```");\n/// ```\npub fn f() {}\n'
  # Round 12's F2: a raw identifier IS its name. `r#assert_eq` rebinds the
  # trusted macro exactly as `assert_eq` does, by `macro` or `macro_rules!`.
  opaque_case "r12 F2: a decl_macro rebinding r#assert_eq inside a doctest" included_by_rust_opaque:src/lib.rs:raw_text:doctest: \
    '/// ```\n/// macro r#assert_eq($s:expr) { other::documented!($s) }\n/// assert_eq!("```\\nmacro_rules! m { ($i:ident) => { $i!(\\"data.txt\\") } }\\nlet _ = m!(include);\\n```");\n/// ```\npub fn f() {}\n'
  opaque_case "r12 F2: a macro_rules! rebinding r#assert_eq inside a doctest" included_by_rust_opaque:src/lib.rs:raw_text:doctest: \
    '/// ```\n/// macro_rules! r#assert_eq { ($s:expr) => { other::documented!($s) } }\n/// assert_eq!("```\\nmacro_rules! m { ($i:ident) => { $i!(\\"data.txt\\") } }\\nlet _ = m!(include);\\n```");\n/// ```\npub fn f() {}\n'
  # Round 11's F2: ANY whole-word macro_use revokes trust (a cfg_attr
  # spelling here), and in ANY tracked file -- `include!` compiles a file of
  # any name as Rust source.
  for V in cfgattr incl; do
    X="$SANDBOX/r11-macro-use-$V"; mkrepo "$X"
    printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$X/Cargo.toml"
    if [ "$V" = cfgattr ]; then
      printf '#[cfg_attr(all(), macro_use)]\nextern crate dep;\nmod x;\n' | wr "$X/src/lib.rs"
    else
      printf 'include!("prelude.in");\nmod x;\n' | wr "$X/src/lib.rs"
      printf '#[macro_use] extern crate dep;\n' | wr "$X/src/prelude.in"
    fi
    printf 'assert_eq!("```\nmacro_rules! m { ($i:ident) => { $i!(\\"data.txt\\") } }\nlet _ = m!(include);\n```");\npub fn f() {}\n' | wr "$X/src/x.rs"
    printf 'a\n' | wr "$X/src/data.txt"
    cm "$X" base; B_X="$(git -C "$X" rev-parse HEAD)"
    printf 'b\n' | wr "$X/src/data.txt"
    cm "$X" head
    run_check "$X" --base "$B_X" --head HEAD --code-paths '["src/**/*.rs"]'
    expect_file "include: r11 F2 ($V): a macro_use elsewhere revokes the trusted name" src/data.txt unknown included_by_rust_opaque:src/x.rs:raw_text:doctest:
  done
fi

# ---- section: include-link -------------------------------------------------------
if mc_sec include-link; then
  echo "== include-link"
  # Round 11's F4: a symlink in the include graph. An include of a tracked
  # link is opaque; any file a link names (directly, or through a linked
  # directory, or as a linked module) is unknown, whatever reads the link.
  Y="$SANDBOX/r11-symlink-include"; mkrepo "$Y"
  printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$Y/Cargo.toml"
  printf '#![doc = include_str!("../docs/link.md")]\npub fn f() {}\n' | wr "$Y/src/lib.rs"
  printf '# T\n\nprose\n' | wr "$Y/docs/real.md"
  ln -s real.md "$Y/docs/link.md"
  cm "$Y" base; B_Y="$(git -C "$Y" rev-parse HEAD)"
  printf '# T\n\nprose, edited\n' | wr "$Y/docs/real.md"
  cm "$Y" head
  run_check "$Y" --base "$B_Y" --head HEAD --code-paths '["src/**/*.rs"]'
  expect_file "include: r11 F4: an include of a tracked symlink is opaque" docs/real.md unknown included_by_rust_opaque:src/lib.rs:include_through_symlink:../docs/link.md
  Y="$SANDBOX/r11-symlink-alias"; mkrepo "$Y"
  printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$Y/Cargo.toml"
  printf 'mod gen;\npub fn f() {}\n' | wr "$Y/src/lib.rs"
  printf 'pub fn g() {}\n' | wr "$Y/gen/gen.txt"
  ln -s ../gen/gen.txt "$Y/src/gen.rs"
  printf 'x\n' | wr "$Y/assets/inner/a.txt"
  ln -s ../assets "$Y/src/assets"
  printf 'plain\n' | wr "$Y/docs/plain.md"
  cm "$Y" base; B_Y="$(git -C "$Y" rev-parse HEAD)"
  printf 'pub fn g() { compile_error!("x"); }\n' | wr "$Y/gen/gen.txt"
  printf 'y\n' | wr "$Y/assets/inner/a.txt"
  printf 'plainer\n' | wr "$Y/docs/plain.md"
  cm "$Y" head
  run_check "$Y" --base "$B_Y" --head HEAD --code-paths '["src/**/*.rs"]'
  expect_file "include: r11 F4: a module file named only by a linked .rs is unknown" gen/gen.txt unknown aliased_by_symlink:src/gen.rs
  expect_file "include: r11 F4: a file under a linked directory is unknown" assets/inner/a.txt unknown aliased_by_symlink:src/assets
  expect_file "include: r11 F4: a file no link names stays non_executable" docs/plain.md non_executable outside_code_paths
  # Round 13's F1/F2: NO link target is proven to leave the repository. One
  # that climbs above the root may come back in under the name the checkout
  # is cloned as (a default clone of `widget` resolves `../../widget/x` into
  # itself; a worktree under another name does not), however many times it
  # climbs; an absolute one that misses this root here may hit it elsewhere.
  # Each such link aliases every candidate. (Round 12's F5 fixture lives
  # here now: docs/ is made first, and the link exists and changes.)
  for V in away clone hops absolute; do
    Y="$SANDBOX/r13-above-root-$V"; mkrepo "$Y"
    printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$Y/Cargo.toml"
    printf 'mod gen;\npub fn f() {}\n' | wr "$Y/src/lib.rs"
    printf 'pub fn g() {}\n' | wr "$Y/gen/gen.txt"
    mkdir -p "$Y/docs"
    case "$V" in
      away) T=../../outside-the-repo; L=docs/away.md ;;
      clone) T=../../widget/gen/gen.txt; L=src/gen.rs ;;
      hops) N="$(basename "$Y")"; T=../..; for _ in 1 2 3 4 5 6; do T="$T/$N/.."; done; T="$T/$N/gen/gen.txt"; L=src/gen.rs ;;
      absolute) T=/nonexistent-qontinui-r13/gen/gen.txt; L=src/gen.rs ;;
    esac
    ln -s "$T" "$Y/$L"
    [ -L "$Y/$L" ] && ok "include: r13 ($V): the link fixture exists" || bad "include: r13 ($V): the link fixture exists" "$Y/$L was not created"
    printf 'plain\n' | wr "$Y/docs/plain.md"
    cm "$Y" base; B_Y="$(git -C "$Y" rev-parse HEAD)"
    printf 'pub fn g() { compile_error!("x"); }\n' | wr "$Y/gen/gen.txt"
    printf 'plainer\n' | wr "$Y/docs/plain.md"
    [ "$V" = away ] && { rm "$Y/docs/away.md"; ln -s ../../outside-the-repo/elsewhere "$Y/docs/away.md"; }
    cm "$Y" head
    run_check "$Y" --base "$B_Y" --head HEAD --code-paths '["src/**/*.rs"]'
    expect_file "include: r13 ($V): a link above the root aliases its candidates" gen/gen.txt unknown "aliased_by_symlink:$L"
    expect_file "include: r13 ($V): ... every one of them" docs/plain.md unknown "aliased_by_symlink:$L"
    [ "$V" = away ] && expect_file "include: r12 F5: the leaves-the-repo link fixture is judged itself" docs/away.md unknown symlink_outside_code_paths
  done
fi

# ---- section: include-r14 --------------------------------------------------------
if mc_sec include-r14; then
  echo "== include-r14"
  # Round 14's F1: a link target that reaches nothing this side tracks by
  # that exact spelling is an alias, as the include arm already treats it:
  # on NTFS / APFS `../GEN/gen.txt` and `../gen/gen.txt.` ARE gen/gen.txt.
  # Round 14's F2: a backslash or drive spelling (`\rooted`, `\\UNC\..`,
  # `C:..`) is never followed as a plain relative path.
  for V in fold-case fold-dot win-rooted win-unc win-drive win-rel; do
    Y="$SANDBOX/r14-$V"; mkrepo "$Y"
    printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$Y/Cargo.toml"
    printf 'mod gen;\npub fn f() {}\n' | wr "$Y/src/lib.rs"
    printf 'pub fn g() {}\n' | wr "$Y/gen/gen.txt"
    case "$V" in
      fold-case) T=../GEN/gen.txt ;;
      fold-dot) T=../gen/gen.txt. ;;
      win-rooted) T='\Projects\widget\gen\gen.txt' ;;
      win-unc) T='\\localhost\C$\Projects\widget\gen\gen.txt' ;;
      win-drive) T='C:../gen/gen.txt' ;;
      win-rel) T='..\gen\gen.txt' ;;
    esac
    ln -s "$T" "$Y/src/gen.rs"
    [ -L "$Y/src/gen.rs" ] && ok "include: r14 ($V): the link fixture exists" || bad "include: r14 ($V): the link fixture exists" "not created"
    printf 'plain\n' | wr "$Y/docs/plain.md"
    cm "$Y" base; B_Y="$(git -C "$Y" rev-parse HEAD)"
    printf 'pub fn g() { compile_error!("x"); }\n' | wr "$Y/gen/gen.txt"
    printf 'plainer\n' | wr "$Y/docs/plain.md"
    cm "$Y" head
    run_check "$Y" --base "$B_Y" --head HEAD --code-paths '["src/**/*.rs"]'
    expect_file "include: r14 ($V): a link to an untracked spelling aliases its candidates" gen/gen.txt unknown aliased_by_symlink:src/gen.rs
    expect_file "include: r14 ($V): ... every one of them" docs/plain.md unknown aliased_by_symlink:src/gen.rs
  done
  # Round 15's F2: two tracked spellings one case-folding or normalising
  # filesystem (NTFS, APFS) makes ONE entry. The exact spelling being tracked
  # proves nothing there, so the shared resolver fails closed on a twin: the
  # link net aliases every candidate, the include arm is unresolved.
  Y="$SANDBOX/r15-fold-link"; mkrepo "$Y"
  printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$Y/Cargo.toml"
  printf 'mod gen;\npub fn f() {}\n' | wr "$Y/src/lib.rs"
  printf 'pub fn g() {}\n' | wr "$Y/gen/gen.txt"; printf 'pub fn g() {}\n' | wr "$Y/GEN/gen.txt"
  ln -s ../gen/gen.txt "$Y/src/gen.rs"
  cm "$Y" base; B_Y="$(git -C "$Y" rev-parse HEAD)"
  printf 'pub fn g() { compile_error!("x"); }\n' | wr "$Y/GEN/gen.txt"
  cm "$Y" head
  run_check "$Y" --base "$B_Y" --head HEAD --code-paths '["src/**/*.rs"]'
  expect_file "include: r15 F2: a link whose target has a case-fold twin aliases the twin" GEN/gen.txt unknown aliased_by_symlink:src/gen.rs
  for V in case nfd; do
    Z="$SANDBOX/r15-fold-include-$V"; mkrepo "$Z"
    printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$Z/Cargo.toml"
    if [ "$V" = case ]; then N1=data.txt; N2=DATA.txt; else N1="$(printf 'caf\303\251.txt')"; N2="$(printf 'cafe\314\201.txt')"; fi
    printf 'pub const X: &str = include_str!("%s");\n' "$N1" | wr "$Z/src/lib.rs"
    printf 'a\n' | wr "$Z/src/$N1"; printf 'a\n' | wr "$Z/src/$N2"
    cm "$Z" base; B_Z="$(git -C "$Z" rev-parse HEAD)"
    printf 'b\n' | wr "$Z/src/$N2"
    cm "$Z" head
    run_check "$Z" --base "$B_Z" --head HEAD --code-paths '["src/**/*.rs"]'
    expect_file "include: r15 F2 ($V): an include whose target has a fold twin is unresolved" "src/$N2" unknown included_by_rust_opaque:src/lib.rs:unresolved_path:
  done
fi

# ---- section: include-resolve ----------------------------------------------------
if mc_sec include-resolve; then
  echo "== include-resolve"
  # Round 13's F3: rustc compiles `r#include_str!` as the std macro, and the
  # tokenizer folds the raw identifier, so it is a PLAIN include, resolved
  # and judged like any other.
  Z="$SANDBOX/r13-raw-include"; mkrepo "$Z"
  printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$Z/Cargo.toml"
  printf 'pub const X: &str = r#include_str!("data.txt");\n' | wr "$Z/src/lib.rs"
  printf 'a\n' | wr "$Z/src/data.txt"
  cm "$Z" base; B_Z="$(git -C "$Z" rev-parse HEAD)"
  printf 'b\n' | wr "$Z/src/data.txt"
  cm "$Z" head
  run_check "$Z" --base "$B_Z" --head HEAD --code-paths '["src/**/*.rs"]'
  expect_file "include: r13 F3: r#include_str! is a plain include" src/data.txt executable included_by_rust:src/lib.rs
  # Round 12's F1: `..` is applied on disk AFTER links are followed, so a
  # string normpath of `sub/../data.txt` names the wrong file when `sub` is
  # a link; and `..` out of a directory the tree does not hold is unknown.
  Z="$SANDBOX/r12-dotdot"; mkrepo "$Z"
  printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$Z/Cargo.toml"
  printf 'pub const X: &str = include_str!("sub/../data.txt");\n' | wr "$Z/src/lib.rs"
  printf 'k\n' | wr "$Z/other/deep/keep.txt"
  ln -s ../other/deep "$Z/src/sub"
  printf 'a\n' | wr "$Z/src/data.txt"; printf 'a\n' | wr "$Z/other/data.txt"
  cm "$Z" base; B_Z="$(git -C "$Z" rev-parse HEAD)"
  printf 'b\n' | wr "$Z/other/data.txt"
  cm "$Z" head
  run_check "$Z" --base "$B_Z" --head HEAD --code-paths '["src/**/*.rs"]'
  expect_file "include: r12 F1: sub/../x through a linked sub is opaque" other/data.txt unknown included_by_rust_opaque:src/lib.rs:include_through_symlink:sub/../data.txt
  Z="$SANDBOX/r12-dotdot-untracked"; mkrepo "$Z"
  printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$Z/Cargo.toml"
  printf 'pub const X: &str = include_str!("nowhere/../data.txt");\n' | wr "$Z/src/lib.rs"
  printf 'a\n' | wr "$Z/src/data.txt"
  cm "$Z" base; B_Z="$(git -C "$Z" rev-parse HEAD)"
  printf 'b\n' | wr "$Z/src/data.txt"
  cm "$Z" head
  run_check "$Z" --base "$B_Z" --head HEAD --code-paths '["src/**/*.rs"]'
  expect_file "include: r12 F1: .. out of an untracked directory is unknown" src/data.txt unknown included_by_rust_opaque:src/lib.rs:unresolved_path:nowhere/../data.txt
  # Round 12's F3: a relative link that climbs out of the checkout and comes
  # back in by the checkout's own directory name still names gen/gen.txt.
  Z="$SANDBOX/r12-reenter"; mkrepo "$Z"
  printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$Z/Cargo.toml"
  printf 'mod gen;\npub fn f() {}\n' | wr "$Z/src/lib.rs"
  printf 'pub fn g() {}\n' | wr "$Z/gen/gen.txt"
  ln -s "../../$(basename "$Z")/gen/gen.txt" "$Z/src/gen.rs"
  cm "$Z" base; B_Z="$(git -C "$Z" rev-parse HEAD)"
  printf 'pub fn g() { compile_error!("x"); }\n' | wr "$Z/gen/gen.txt"
  cm "$Z" head
  run_check "$Z" --base "$B_Z" --head HEAD --code-paths '["src/**/*.rs"]'
  expect_file "include: r12 F3: a link re-entering the checkout by its own name aliases its target" gen/gen.txt unknown aliased_by_symlink:src/gen.rs
  # Round 12's F4: a link retargeted between base and head names BOTH
  # targets; an edit to the base-side one is still read through the link.
  Z="$SANDBOX/r12-retarget"; mkrepo "$Z"
  printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$Z/Cargo.toml"
  printf 'mod gen;\n' | wr "$Z/src/lib.rs"
  printf 'pub fn g() {}\n' | wr "$Z/gen/a.txt"; printf 'pub fn g() {}\n' | wr "$Z/gen/b.txt"
  ln -s ../gen/a.txt "$Z/src/gen.rs"
  cm "$Z" base; B_Z="$(git -C "$Z" rev-parse HEAD)"
  printf 'pub fn g() { compile_error!("x"); }\n' | wr "$Z/gen/a.txt"
  rm "$Z/src/gen.rs"; ln -s ../gen/b.txt "$Z/src/gen.rs"
  cm "$Z" head
  run_check "$Z" --base "$B_Z" --head HEAD --code-paths '["src/**/*.rs"]'
  expect_file "include: r12 F4: a retargeted link still names its base-side target" gen/a.txt unknown aliased_by_symlink:src/gen.rs
fi

# ---- section: include-plain ------------------------------------------------------
if mc_sec include-plain; then
  echo "== include-plain"
  # ... and the shapes those rules must leave alone: a raw identifier, an
  # attribute, a plain #[path] to a .rs module, a macro forwarding its tts.
  O="$SANDBOX/r8-neg"; mkrepo "$O"
  printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$O/Cargo.toml"
  printf '#![allow(dead_code)]\n#[path = "m.rs"]\nmod m;\n#[derive(Debug)]\npub struct S { pub r#type: u8 }\nmacro_rules! fwd { ($($t:tt)*) => { m::g($($t)*) }; }\npub fn f() { fwd!(1); }\npub const X: &str = include_str!("data.txt");\n' | wr "$O/src/lib.rs"
  printf 'pub fn g(_: u8) {}\n' | wr "$O/src/m.rs"
  printf 'a\n' | wr "$O/src/data.txt"; printf 'a\n' | wr "$O/docs/other.md"
  cm "$O" base; B_O="$(git -C "$O" rev-parse HEAD)"
  printf 'b\n' | wr "$O/src/data.txt"; printf 'b\n' | wr "$O/docs/other.md"
  cm "$O" head
  run_check "$O" --base "$B_O" --head HEAD --code-paths '["src/**/*.rs"]'
  expect_file "include: r8 negatives: a plain include beside them still reads executable" src/data.txt executable included_by_rust:src/lib.rs
  expect_file "include: r8 negatives: r#ident, attributes, #[path] to .rs, tt forwarding are not opaque" docs/other.md non_executable outside_code_paths
  # A shadowed std macro name is not the std macro.
  opaque_case "an include handed to a locally redefined assert_eq!" included_by_rust_opaque:src/lib.rs:include_in_macro_arguments:include_str \
    'macro_rules! assert_eq { ($a:expr, $b:expr) => {}; }\npub fn t() { assert_eq!(include_str!("data.txt"), "a\\n"); }\n'
  # ... but a std expression macro's argument is data, read as a plain include.
  O="$SANDBOX/std-macro-arg"; mkrepo "$O"
  printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$O/Cargo.toml"
  printf 'pub fn t() { assert_eq!(include_str!("data.txt"), "a\\n"); }\n' | wr "$O/src/lib.rs"
  printf 'a\n' | wr "$O/src/data.txt"; printf 'a\n' | wr "$O/docs/other.md"
  cm "$O" base; B_O="$(git -C "$O" rev-parse HEAD)"
  printf 'b\n' | wr "$O/src/data.txt"; printf 'b\n' | wr "$O/docs/other.md"
  cm "$O" head
  run_check "$O" --base "$B_O" --head HEAD --code-paths '["src/**/*.rs"]'
  expect_file "include: an include inside assert_eq! is a plain include" src/data.txt executable included_by_rust:src/lib.rs
  expect_file "include: an include inside assert_eq! is not opaque" docs/other.md non_executable outside_code_paths
  # Round-6 finding 2: a README the crate doc-includes holds the doctest.
  O="$SANDBOX/readme-doctest"; mkrepo "$O"
  printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$O/Cargo.toml"
  printf '#![doc = include_str!("../README.md")]\npub fn f() {}\n' | wr "$O/src/lib.rs"
  printf 'Readme.\n\n```\nlet s = include_str!("data.txt");\nassert_eq!(s, "a\\n");\n```\n' | wr "$O/README.md"
  printf 'a\n' | wr "$O/src/data.txt"; printf 'a\n' | wr "$O/docs/other.md"
  cm "$O" base; B_O="$(git -C "$O" rev-parse HEAD)"
  printf 'b\n' | wr "$O/src/data.txt"; printf 'b\n' | wr "$O/docs/other.md"
  cm "$O" head
  run_check "$O" --base "$B_O" --head HEAD --code-paths '["src/**/*.rs"]'
  expect_file "include: a doctest include inside a doc-included README makes an included file unknown" src/data.txt unknown \
    included_by_rust_opaque:src/lib.rs:attribute_included_text:README.md:include_str
  expect_file "include: a doctest include inside a doc-included README makes every non-code file unknown" docs/other.md unknown \
    included_by_rust_opaque:src/lib.rs:attribute_included_text:README.md:include_str
fi

# ---- section: include-readers ----------------------------------------------------
if mc_sec include-readers; then
  echo "== include-readers"
  # Round-6 finding 3: git grep -I skips a blob git deems binary -- a `-diff`
  # attribute, or a NUL byte -- which hid even a PLAIN include.
  O="$SANDBOX/binary-rs"; mkrepo "$O"
  printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$O/Cargo.toml"
  printf 'src/gen.rs -diff\n' | wr "$O/.gitattributes"
  printf 'mod gen;\nmod zero;\n' | wr "$O/src/lib.rs"
  printf 'pub const X: &str = include_str!("data.txt");\n' | wr "$O/src/gen.rs"
  printf '// nul:\000\npub const Y: &str = include_str!("data2.txt");\n' | wr "$O/src/zero.rs"
  printf 'a\n' | wr "$O/src/data.txt"; printf 'a\n' | wr "$O/src/data2.txt"
  cm "$O" base; B_O="$(git -C "$O" rev-parse HEAD)"
  printf 'b\n' | wr "$O/src/data.txt"; printf 'b\n' | wr "$O/src/data2.txt"
  cm "$O" head
  run_check "$O" --base "$B_O" --head HEAD --code-paths '["src/**/*.rs"]'
  expect_file "include: a plain include in a -diff (git-binary) .rs file is still read" src/data.txt executable included_by_rust:src/gen.rs
  expect_file "include: a plain include in a .rs file holding a NUL byte is still read" src/data2.txt executable included_by_rust:src/zero.rs
  # A color.grep=always config wraps every git grep name in escape codes,
  # which then match no tracked path.
  GIT_CONFIG_COUNT=4 GIT_CONFIG_KEY_3=color.grep GIT_CONFIG_VALUE_3=always     run_check "$O" --base "$B_O" --head HEAD --code-paths '["src/**/*.rs"]'
  expect_file "include: a color.grep=always config does not hide an include" src/data.txt executable included_by_rust:src/gen.rs
  # An include of a file under OUT_DIR names the build directory, never a
  # repository file: it is not opaque, so the repo's other files are judged.
  O="$SANDBOX/outdir"; mkrepo "$O"
  printf '[package]\nname = "o"\nversion = "0.1.0"\n' | wr "$O/Cargo.toml"
  printf 'include!(concat!(env!("OUT_DIR"), "/gen.rs"));\npub const X: &str = include_str!("data.txt");\n' | wr "$O/src/lib.rs"
  printf 'a\n' | wr "$O/src/data.txt"; printf 'a\n' | wr "$O/docs/other.md"
  cm "$O" base; B_O="$(git -C "$O" rev-parse HEAD)"
  printf 'b\n' | wr "$O/src/data.txt"; printf 'b\n' | wr "$O/docs/other.md"
  cm "$O" head
  run_check "$O" --base "$B_O" --head HEAD --code-paths '["src/**/*.rs"]'
  expect_file "include: beside an OUT_DIR include, a plain include still reads executable" src/data.txt executable included_by_rust:src/lib.rs
  expect_file "include: an OUT_DIR include is not opaque" docs/other.md non_executable outside_code_paths
  opaque_case "include_dir!" compile_time_reader:include_dir:src/lib.rs \
    'pub static D: include_dir::Dir = include_dir::include_dir!("src");\n'
  opaque_case "a rust-embed #[folder]" "compile_time_reader:#[folder = ..]:src/lib.rs" \
    '#[derive(Embed)]\n#[folder = "src/"]\npub struct Assets;\n'
  opaque_case "an askama #[template(path)]" "compile_time_reader:#[template(path = ..)]:src/lib.rs" \
    '#[derive(Template)]\n#[template(path = "data.txt")]\npub struct T;\n'
  opaque_case "sqlx query_file!" compile_time_reader:query_file:src/lib.rs \
    'pub async fn q() { let _ = sqlx::query_file!("src/data.txt"); }\n'
fi

# ---- section: coord ------------------------------------------------------------
if mc_sec coord; then
  echo "== coord"
  if ensure_fixture; then
    export EXEC_EQ_COORD_URL="$FIX_URL" COORD_DEVICE_JWT="$FAKE_JWT" QONTINUI_TENANT_ID="$TENANT"
    wr "$SANDBOX/corpus.py" <<'PYEOF'
"""The parity corpus: every input pair of coord's ne6-ne20 proof tests
(crates/coord/src/pr_merge/migration_classifier.rs, read at qontinui-coord
9e5bec3dc) with coord's recorded verdict, plus this client's own rows. ne1-ne5
and ne13 hold no pair this route sees on its own (a whole-file classifier read,
qontinui-web's 140-line revision text, a base-less change); ne10's and ne12's
identical-side pairs never reach the route, because an unchanged file is not
in a diff."""
import hashlib, json, os, sys

MIG = "migration"
TINY = '"""doc one"""\n\nfrom alembic import op\n\nSQL = "DROP TABLE t"\n\n\ndef upgrade() -> None:\n    """up doc"""\n    op.execute(SQL)  # run it\n'
E = "\u00e9"
ROWS = []

def row(name, path, base, head, verdicts, want, reason):
    ROWS.append((name, path, base, head, verdicts, want, reason))

def ne(name, base, head, proven):
    row(name, "a/alembic/versions/%s.py" % name, base, head, {"any": proven},
        "non_executable" if proven else "executable",
        "coord_proof:migration" if proven else "coord_refused_proof:migration")

ne("ne6", TINY, TINY.replace("doc one", "doc one, rewritten\nacross lines").replace("up doc", "a different up doc")
   .replace("# run it", "# new comment").replace("from alembic import op\n", "# a new comment line\nfrom alembic import op\n\n\n"), True)
REMOVED = TINY.replace('    """up doc"""\n', "")
ne("ne7_removed", TINY, REMOVED, False)
ne("ne7_added", REMOVED, TINY, False)
B8 = 'def upgrade():\n    op.execute(\n        "CREATE TABLE a (x int)"\n    )\n'
ne("ne8_operand", B8, B8.replace("TABLE a", "TABLE b"), False)
ne("ne8_join", '"a".join([])\nx = 1\n', '"b".join([])\nx = 1\n', False)
ne("ne8_concat", '"a" "b"\n', '"a" "c"\n', False)
ne("ne9", 'f"{op.execute(\'x\')}"\n', 'f"{op.execute(\'y\')}"\n', False)
ne("ne10", 'x = \\\n"a"\n', 'x = \\\n"b"\n', False)
C11 = "# -*- coding: utf-8 -*-\nx = '" + E + "'\n"
ne("ne11_cookie", C11, "# -*- coding: latin-1 -*-\nx = '" + E + "'\n", False)
ne("ne11_comment", C11, C11 + "# unrelated\n", True)
for i, reader in enumerate(["op.execute(__doc__)", "op.execute(upgrade.__doc__)", "op.execute(inspect.getdoc(upgrade))",
                            "op.execute(open(__file__).read())", "op.execute(getattr(upgrade, 'x'))"]):
    b = '"""CREATE TABLE a (x int)"""\n\ndef upgrade():\n    %s\n' % reader
    ne("ne14_%d" % i, b, b.replace("TABLE a", "TABLE b"), False)
for i, reader in enumerate(['op.execute(f"{upgrade.__doc__}")', 'op.execute("{0.__doc__}".format(upgrade))',
                            "op.execute(context.script.get_revision(revision).longdoc)",
                            "op.execute(traceback.extract_stack()[-1].line)"]):
    b = '"""CREATE TABLE a (x int)"""\n\ndef upgrade():\n    %s\n' % reader
    ne("ne15_%d" % i, b, b.replace("TABLE a", "TABLE b"), False)
B16 = '"""doc"""\nx = 1\n'
BS = "\\"
for i, bad in enumerate(['"""C:' + BS + 'Us' + 'ers' + BS + 'x"""\nx = 1\n', '"""see \\N{BOGUS}"""\nx = 1\n',
                         '"""hex \\x4"""\nx = 1\n', 'b"""caf' + E + '"""\nx = 1\n', '"""a\x00b"""\nx = 1\n']):
    ne("ne16_%d" % i, B16, bad, False)
ne("ne16_raw", 'r"""C:\\old"""\nx = 1\n', 'r"""C:\\new\\N{X}"""\nx = 1\n', True)
ne("ne17_moved", '"""doc"""\n# -*- coding: latin-1 -*-\nx = "' + E + '"\n',
   '\n# -*- coding: latin-1 -*-\n"""doc"""\nx = "' + E + '"\n', False)
ne("ne17_trailing", '# -*- coding: latin-1 -*-\nimport alembic\nx = "' + E + '"\n',
   'import alembic  # -*- coding: latin-1 -*-\nx = "' + E + '"\n', False)
ne("ne18_cr", B16, '"doc\rmore"\nx = 1\n', False)
for i, reader in enumerate(["op.execute(upgrade.__\uff44oc__)", "doctest.run_docstring_examples(upgrade, {})"]):
    b = '"""CREATE TABLE a (x int)"""\n\ndef upgrade():\n    %s\n' % reader
    ne("ne18_%d" % i, b, b.replace("TABLE a", "TABLE b"), False)
ne("ne18_crlf", '"""doc"""\r\nx = 1\r\n', '"""other doc"""\r\nx = 1\r\n', True)
for i, (cookie, proven) in enumerate([("# -*- coding: ascii -*-", False), ("# vim: set fileencoding=cp1252 :", False),
                                       ("# -*- coding: utf-8 -*-", True), ("# coding=UTF_8", True),
                                       ("# coding: utf-8-sig", True)]):
    b = cookie + '\nrevision = "x"\n"""doc"""\nx = 1\n'
    ne("ne19_%d" % i, b, b.replace('"""doc"""', '"""doc ' + E + '"""'), proven)
B19 = '# coding: utf8-sig\nx = 1\n"""doc"""\n'
ne("ne19_utf8sig", B19, B19.replace("doc", "other"), False)
B19G = '# see the coding guide\nx = 1\n"""doc"""\n'
ne("ne19_guide", B19G, B19G.replace('"doc"', '"other"'), True)
ne("ne19_empty", "# c\n", "", False)
for i, bad in enumerate(['B"""caf' + E + '"""\nx = 1\n', 'F"{op.execute(\'x\')}"\nx = 1\n']):
    ne("ne20_%d" % i, B16, bad, False)
for i, reader in enumerate(["if w[0].lineno > 20: op.drop_table('users')",
                            "op.execute(string.Formatter().vformat('{0.__doc__}', (upgrade,), {}))"]):
    b = '"""CREATE TABLE a (x int)"""\n\ndef upgrade():\n    %s\n' % reader
    ne("ne20_%d" % (i + 2), b, b.replace("TABLE a", "TABLE b"), False)

# This client's own rows. Each `true` is coord's REAL answer for that pair: the
# edit touches only a docstring, a comment or a statement string, and coord has
# no >>> rule, no shebang rule and no directive rule -- which is exactly what
# the client adds on top.
row("doctest_gain", "pkg/doctest_gain.py", '"""doc"""\nx = 1\n', '"""doc\n\n>>> 1 + 1\n2\n"""\nx = 1\n',
    {"any": True}, "executable", "doctest_marker")
row("doctest_untouched", "pkg/doctest_untouched.py",
    '"""keep\n\n>>> f()\n1\n"""\n\n\ndef f():\n    """old"""\n    return 1\n',
    '"""keep\n\n>>> f()\n1\n"""\n\n\ndef f():\n    """new"""\n    return 1\n',
    {"any": True}, "non_executable", "coord_proof:general")
row("doctest_comment", "pkg/doctest_comment.py", "# a >>> b, old\nx = 1\n", "# a >>> b, new\nx = 1\n",
    {"any": True}, "non_executable", "coord_proof:general")
NAME = '"""%s"""\nif __name__ == "__main__":\n    print(1)\n'
row("name_general", "pkg/name_reader.py", NAME % "doc", NAME % "other doc",
    {"general": True, "migration": False}, "non_executable", "coord_proof:general")
row("name_migration", "a/alembic/versions/name_reader.py", NAME % "doc", NAME % "other doc",
    {"general": True, "migration": False}, "executable", "coord_refused_proof:migration")
row("django_migration", "app/migrations/0001_initial.py", '"""doc"""\nx = 1\n', '"""other"""\nx = 1\n',
    {"migration": True, "general": False}, "non_executable", "coord_proof:migration")
row("shebang", "tools/run.py", '#!/usr/bin/env python3\n"""doc"""\nx = 1\n', '#!/usr/bin/env python2\n"""doc"""\nx = 1\n',
    {"any": True}, "executable", "shebang_changed")
row("directive", "pkg/noqa.py", "import json  # noqa: F401\n", "import json\n",
    {"any": True}, "executable", "directive_comment")
row("shebang_language", "bin/tool", '#!/usr/bin/env python3\n"""doc"""\n', '#!/usr/bin/env python3\n"""other"""\n',
    {"any": False}, "executable", "coord_refused_proof:general")


def is_migration_path(path):
    norm = path.replace("\\", "/").lower()
    return "alembic/versions/" in norm or norm.startswith("migrations/") or "/migrations/" in norm


def sha(text):
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def main():
    cmd = sys.argv[1]
    if cmd == "write":
        repo, side = sys.argv[2], sys.argv[3]
        for name, path, base, head, *_ in ROWS:
            full = os.path.join(repo, *path.split("/"))
            os.makedirs(os.path.dirname(full), exist_ok=True)
            with open(full, "wb") as fh:
                fh.write((base if side == "base" else head).encode("utf-8"))
    elif cmd == "table":
        with open(sys.argv[2], "w", encoding="utf-8") as fh:
            json.dump({"%s|%s|%s" % (p, sha(b), sha(h)): v for _, p, b, h, v, *_ in ROWS}, fh)
    elif cmd == "check":
        report = json.load(open(sys.argv[2], encoding="utf-8"))
        seen = {}
        for line in open(sys.argv[3], encoding="utf-8"):
            rec = json.loads(line)
            if rec.get("route") == "python":
                seen.setdefault(rec["path"], set()).add(rec["profile"])
        files = {f["path"]: f for f in report["files"]}
        passed = failed = 0
        for name, path, _b, _h, _v, want, reason in ROWS:
            got = files.get(path)
            profile = "migration" if is_migration_path(path) else "general"
            problems = []
            if got is None:
                problems.append("no verdict")
            else:
                if got["verdict"] != want:
                    problems.append("verdict %s, want %s" % (got["verdict"], want))
                if not got["reason"].startswith(reason):
                    problems.append("reason %s, want %s*" % (got["reason"], reason))
            if seen.get(path) != {profile}:
                problems.append("coord saw profiles %s, want {%s}" % (sorted(seen.get(path, [])), profile))
            if problems:
                failed += 1
                print("  FAIL coord: corpus %s (%s): %s" % (name, path, "; ".join(problems)))
            else:
                passed += 1
                print("  ok   coord: corpus %s (%s) -> %s, sent as %s" % (name, path, want, profile))
        print("CORPUS rows=%d pass=%d fail=%d" % (len(ROWS), passed, failed))


main()
PYEOF
    CORPUS_N="$(native_path_m "$SANDBOX/corpus.py")"
    C="$SANDBOX/corpus"; mkrepo "$C"
    "$PY" "$CORPUS_N" write "$(native_path_m "$C")" base && cm "$C" base; BASE_C="$(git -C "$C" rev-parse HEAD)"
    "$PY" "$CORPUS_N" write "$(native_path_m "$C")" head && cm "$C" head
    "$PY" "$CORPUS_N" table "$(native_path_m "$FIX_DIR/table.json")"
    set_cfg '{"owned": ["acme/widget"]}'; clear_log
    run_check "$C" --base "$BASE_C" --head HEAD --json --code-paths '["**"]'
    printf '%s' "$OUT" > "$SANDBOX/corpus-report.json"
    CORPUS_OUT="$("$PY" "$CORPUS_N" check "$(native_path_m "$SANDBOX/corpus-report.json")" "$(native_path_m "$FIX_DIR/requests.log")" 2>&1 | tr -d '\r')"
    printf '%s\n' "$CORPUS_OUT" | grep -E '^  (ok  |FAIL) '
    CORPUS_SUM="$(printf '%s\n' "$CORPUS_OUT" | sed -n 's/^CORPUS rows=\([0-9]*\) pass=\([0-9]*\) fail=\([0-9]*\)$/\1 \2 \3/p')"
    if [ -n "$CORPUS_SUM" ]; then
      read -r C_ROWS C_PASS C_FAIL <<<"$CORPUS_SUM"
      PASS=$((PASS + C_PASS)); FAIL=$((FAIL + C_FAIL))
      echo "  (parity corpus: $C_ROWS rows from coord's ne* cases and this client's own)"
      [ "$C_ROWS" -gt 0 ] && [ $((C_PASS + C_FAIL)) -eq "$C_ROWS" ] && ok "coord: the corpus checked every row it holds" \
        || bad "coord: the corpus checked every row it holds" "$CORPUS_SUM"
    else
      bad "coord: the corpus check produced its summary" "${CORPUS_OUT:0:600} / ${ERR:0:300}"
    fi

    S="$SANDBOX/scope"; mkrepo "$S"
    printf 'one\n' | wr "$S/docs/a.md"; printf 'one\n' | wr "$S/src/p.md"
    printf '"""doc"""\nx = 1\n' | wr "$S/pkg/m.py"
    cm "$S" base; BASE_S="$(git -C "$S" rev-parse HEAD)"
    printf 'two\n' | wr "$S/docs/a.md"; printf 'two\n' | wr "$S/src/p.md"
    printf '"""doc, reworded"""\nx = 1\n' | wr "$S/pkg/m.py"
    cm "$S" head
    "$PY" -c 'import hashlib, json, sys
b, h = "\"\"\"doc\"\"\"\nx = 1\n", "\"\"\"doc, reworded\"\"\"\nx = 1\n"
t = json.load(open(sys.argv[1], encoding="utf-8"))
t["pkg/m.py|%s|%s" % (hashlib.sha256(b.encode()).hexdigest(), hashlib.sha256(h.encode()).hexdigest())] = {"any": True}
json.dump(t, open(sys.argv[1], "w", encoding="utf-8"))' "$(native_path_m "$FIX_DIR/table.json")"

    set_cfg '{"owned": ["acme/widget"], "code_paths": ["src/**"]}'; clear_log
    run_check "$S" --base "$BASE_S" --head HEAD
    expect_file "coord: code_paths read from coord put a doc outside" docs/a.md non_executable outside_code_paths
    expect_file "coord: code_paths read from coord put a prompt inside" src/p.md executable inside_code_paths
    expect_file "coord: the Python file rides the same proven credential" pkg/m.py non_executable coord_proof:general
    if grep -q "\"route\": \"scope\", \"repo\": \"acme/widget\", \"tenant\": \"$TENANT\"" "$FIX_DIR/requests.log"; then
      ok "coord: code_paths were read for the origin repo under the proven tenant"
    else
      bad "coord: code_paths were read for the origin repo under the proven tenant" "$(cat "$FIX_DIR/requests.log")"
    fi
    expect_err "coord: the provenance line names fixture mode" "FIXTURE MODE"

    set_cfg '{"owned": ["acme/widget"], "code_paths": []}'
    run_check "$S" --base "$BASE_S" --head HEAD
    expect_file "coord: an undeclared scope makes a doc unknown" docs/a.md unknown code_paths_undeclared
    set_cfg '{"owned": ["acme/widget"], "scope_status": 503}'
    run_check "$S" --base "$BASE_S" --head HEAD
    expect_file "coord: an unreadable scope is unknown, never all" docs/a.md unknown "code_paths_unreadable:coord answered HTTP 503"
    set_cfg '{"owned": [], "code_paths": ["src/**"]}'; clear_log
    run_check "$S" --base "$BASE_S" --head HEAD
    expect_file "coord: a repo no tenant is proven to own has unreadable code_paths" docs/a.md unknown code_paths_unreadable
    expect_file "coord: the auth-only Python route still gets a tenant credential" pkg/m.py non_executable coord_proof:general
    if grep -q '"route": "scope"' "$FIX_DIR/requests.log"; then
      bad "coord: no scope read is attempted under an unproven tenant" "$(cat "$FIX_DIR/requests.log")"
    else
      ok "coord: no scope read is attempted under an unproven tenant"
    fi

    for mode_case in "http_404:coord_http_404" "http_422:coord_http_422" "malformed:coord_bad_answer" "wrong_profile:coord_bad_answer"; do
      set_cfg "{\"owned\": [\"acme/widget\"], \"python_mode\": \"${mode_case%%:*}\"}"
      run_check "$S" --base "$BASE_S" --head HEAD --code-paths '["src/**"]'
      expect_file "coord: a ${mode_case%%:*} answer is unknown, never a verdict" pkg/m.py unknown "${mode_case#*:}"
    done
    set_cfg '{"owned": ["acme/widget"]}'

    DEAD_PORT="$("$PY" -c 'import socket
s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()' | tr -d '\r')"
    EXEC_EQ_COORD_URL="http://127.0.0.1:$DEAD_PORT" run_check "$S" --base "$BASE_S" --head HEAD --code-paths '["nothing/**"]'
    expect_rc "coord: an unreachable coord exits 5" 5
    expect_file "coord: an unreachable coord is unknown" pkg/m.py unknown coord_unreachable
    COORD_DEVICE_JWT="" QONTINUI_TENANT_ID="" run_check "$S" --base "$BASE_S" --head HEAD --code-paths '["src/**"]'
    expect_file "coord: no credential is unknown, never non_executable" pkg/m.py unknown no_coord_credential
    EXEC_EQ_COORD_URL="http://coord.example.invalid" run_check "$S" --base "$BASE_S" --head HEAD --code-paths '["src/**"]'
    expect_file "coord: no bearer is sent to a plain-http non-loopback coord" pkg/m.py unknown "no_coord_credential:coord url http://coord.example.invalid is neither https nor loopback"
    unset EXEC_EQ_COORD_URL COORD_DEVICE_JWT QONTINUI_TENANT_ID
  fi
fi

# ---- section: pack (no real pack needed) ------------------------------------
if mc_sec pack; then
  echo "== pack"
  K="$SANDBOX/pack"; mkrepo "$K"
  printf '// one\nfn a() {}\n' | wr "$K/src/m.rs"; cm "$K" base; BASE_K="$(git -C "$K" rev-parse HEAD)"
  printf '// two\nfn a() {}\n' | wr "$K/src/m.rs"; cm "$K" head
  PYFLAGS=(-S)
  run_check "$K" --base "$BASE_K" --head HEAD
  expect_rc "pack: a missing grammar pack exits 5" 5
  expect_file "pack: a missing grammar pack is unknown, never non_executable" src/m.rs unknown tree_sitter_pack_missing
  FAKE="$SANDBOX/fakepack/tree_sitter_language_pack-9.9.9.dist-info"
  mkdir -p "$FAKE"
  printf 'Metadata-Version: 2.1\nName: tree-sitter-language-pack\nVersion: 9.9.9\n' > "$FAKE/METADATA"
  PYTHONPATH="$(native_path_m "$SANDBOX/fakepack")" run_check "$K" --base "$BASE_K" --head HEAD
  expect_file "pack: a pack at another version is unknown" src/m.rs unknown "tree_sitter_pack_version:9.9.9!=$PIN"
  PYFLAGS=()
fi

# ---- the tree-sitter sections ---------------------------------------------------
if mc_sec rust; then
  echo "== rust"
  if [ "$TS_OK" = 1 ]; then
    T="$SANDBOX/rust"; mkrepo "$T"
    printf '[package]\nname = "t"\nversion = "0.1.0"\n' | wr "$T/Cargo.toml"
    printf '// one\nfn a() {}\n' | wr "$T/c01.rs"
    printf '/// Adds one.\npub fn b(x: i32) -> i32 { x + 1 }\n' | wr "$T/c02.rs"
    printf '/// ```\n/// assert_eq!(b(1), 2);\n/// ```\npub fn c() {}\n' | wr "$T/c03.rs"
    printf '/// Example:\n///\n///     let x = 1;\npub fn d() {}\n' | wr "$T/c04.rs"
    printf 'pub fn e() {}\n' | wr "$T/c05.rs"
    printf '// plain\npub fn f() {}\n' | wr "$T/c06.rs"
    printf 'pub const S: &str = "a b";\n' | wr "$T/c07.rs"
    printf 'pub const R: &str = r#"x"#;\n' | wr "$T/c08.rs"
    printf 'fn g() {}\n' | wr "$T/c09.rs"
    printf 'fn h() {}\n' | wr "$T/c10.rs"
    printf '//! Crate docs.\nfn i() {}\n/// Para\nmod c28;\n' | wr "$T/src/lib.rs"
    printf 'fn j() { /* note */ }\n' | wr "$T/c12.rs"
    printf 'fn  k() {}\n' | wr "$T/c13.rs"
    printf '/// Foo\n///     bar baz\npub fn f() {}\n' | wr "$T/c14.rs"
    printf '/// Foo\n///\n///     bar baz\npub fn f() {}\n' | wr "$T/c15.rs"
    printf '/// - item\n///\n///   ```\n///   let a = 1;\n///\n/// tail\n///   ```\npub fn l() {}\n' | wr "$T/c16.rs"
    printf '/// > ```\n/// > let a = 1;\n/// > ```\npub fn m() {}\n' | wr "$T/c17.rs"
    printf '/// <div>\n/// ```\n/// let a = 1;\n/// ```\n/// </div>\npub fn n() {}\n' | wr "$T/c18.rs"
    printf '/// <!--\n/// ```\n/// let a = 1;\n/// ```\n/// -->\npub fn o() {}\n' | wr "$T/c19.rs"
    printf '/// ```rust`x\n/// foo\n/// ```\n/// let a = 1;\n/// ```\npub fn p() {}\n' | wr "$T/c20.rs"
    printf '/// ```\n#[inline]\n/// let a = 1;\n/// ```\npub fn q() {}\n' | wr "$T/c21.rs"
    printf '/// ```\nmod m {\n    //! let a = 1;\n    //! ```\n}\n' | wr "$T/c22.rs"
    printf '/// ```\n/// ```\302\240\n/// let a = 1;\n/// ```\npub fn r() {}\n' | wr "$T/c23.rs"
    printf '/// ```\n/// ```\034\n/// let a = 1;\n/// ```\npub fn s() {}\n' | wr "$T/c24.rs"
    printf '/// a b\npub fn t() {}\n' | wr "$T/c25.rs"
    printf '/** Para.\n\n     * a\n\n     * let a = 1;\n*/\npub fn u() {}\n' | wr "$T/c26.rs"
    printf '/// Para\n#[doc = "      let a = 1;"]\npub fn v() {}\n' | wr "$T/c27.rs"
    printf '//! Module prose.\npub fn w() {}\n' | wr "$T/src/c28.rs"
    printf '/// Para\nmod mm {\n    //!       let a = 1;\n}\n' | wr "$T/c29.rs"
    printf '/** Para. */\npub fn x() {}\n' | wr "$T/c30.rs"
    printf '/// Para\n// @generated\n///     let a = 1;\npub fn y() {}\n' | wr "$T/c31.rs"
    printf '/// Para\n/**\n *     let a = 1;\n */\npub fn z() {}\n' | wr "$T/c32.rs"
    printf '/// Para.\n// note\n/// More.\npub fn aa() {}\n' | wr "$T/c33.rs"
    printf 'macro_rules! mk {\n    () => {\n        /// Made.\n        pub fn made() {}\n    };\n}\n' | wr "$T/c34.rs"
    printf 'macro_rules! wrap { ($i:item) => { $i }; }\n/// Para\nwrap! { pub fn ab() {} }\n' | wr "$T/c35.rs"
    printf '#[derive(Debug, clap::Parser)]\npub struct Cli {\n    /// Name to greet.\n    pub name: String,\n}\n' | wr "$T/c36.rs"
    printf '#[derive(Debug, Clone)]\npub struct Plain {\n    /// A field.\n    #[serde(default)]\n    pub name: String,\n}\n' | wr "$T/c37.rs"
    printf '/// a b\npub fn ac() {}\n' | wr "$T/c38.rs"
    printf 'mod m {\n    //! Inner prose.\n}\n' | wr "$T/c39.rs"
    printf '//! Tool docs.\n\n/// Main docs.\nfn main() {}\n' | wr "$T/src/bin/tool.rs"
    # Round 33: the include is in a sibling, so c41.rs does not name itself.
    printf 'pub const SELF: &str = include_str!("c41.rs");\n' | wr "$T/c41i.rs"
    printf 'pub const X: u8 = 1;\n// a note\n' | wr "$T/c41.rs"
    printf '/// Prose.\npub fn ad() {\n    //! More.\n    //!\n    //!    assert!(false, "BODY RAN");\n}\n' | wr "$T/c42.rs"
    printf 'pub struct S;\n/// Prose.\nimpl S {\n    //! More.\n    //!\n    //!    assert!(false, "IMPL RAN");\n}\n' | wr "$T/c43.rs"
    printf 'pub struct P;\n#[pymethods]\nimpl P {\n    /// Method doc.\n    pub fn m(&self) {}\n}\n' | wr "$T/c44.rs"
    cm "$T" base; BASE_T="$(git -C "$T" rev-parse HEAD)"
    printf '// one, edited\nfn a() {}\n' | wr "$T/c01.rs"
    printf '/// Adds exactly one.\n/// More prose.\npub fn b(x: i32) -> i32 { x + 1 }\n' | wr "$T/c02.rs"
    printf '/// ```\n/// assert_eq!(b(1), 3);\n/// ```\npub fn c() {}\n' | wr "$T/c03.rs"
    printf '/// Example:\n///\n///     let x = 2;\npub fn d() {}\n' | wr "$T/c04.rs"
    printf '/// Now documented.\npub fn e() {}\n' | wr "$T/c05.rs"
    printf '/// plain\npub fn f() {}\n' | wr "$T/c06.rs"
    printf 'pub const S: &str = "a  b";\n' | wr "$T/c07.rs"
    printf 'pub const R: &str = r##"x"##;\n' | wr "$T/c08.rs"
    printf 'fn g( {}\n' | wr "$T/c09.rs"
    printf '// @generated\nfn h() {}\n' | wr "$T/c10.rs"
    printf '//! Crate docs, reworded.\nfn i() {}\n/// Para\nmod c28;\n' | wr "$T/src/lib.rs"
    printf 'fn j() { /* changed note */ }\n' | wr "$T/c12.rs"
    printf 'fn k() {}\n' | wr "$T/c13.rs"
    printf '/// Foo\n///\n///     bar baz\npub fn f() {}\n' | wr "$T/c14.rs"
    printf '/// Foo\n///     bar baz\npub fn f() {}\n' | wr "$T/c15.rs"
    printf '/// item\n///\n///   ```\n///   let a = 1;\n///\n/// tail\n///   ```\npub fn l() {}\n' | wr "$T/c16.rs"
    printf '/// > ```\n/// > let a = 2;\n/// > ```\npub fn m() {}\n' | wr "$T/c17.rs"
    printf '/// <div>\n///\n/// ```\n/// let a = 1;\n/// ```\n/// </div>\npub fn n() {}\n' | wr "$T/c18.rs"
    printf '/// Note:\n/// ```\n/// let a = 1;\n/// ```\npub fn o() {}\n' | wr "$T/c19.rs"
    printf '/// ```rust`x\n/// foo\n/// ```\n/// let a = 2;\n/// ```\npub fn p() {}\n' | wr "$T/c20.rs"
    printf '/// ```\n#[inline]\n/// let a = 2;\n/// ```\npub fn q() {}\n' | wr "$T/c21.rs"
    printf '/// ```\nmod m {\n    //! let a = 2;\n    //! ```\n}\n' | wr "$T/c22.rs"
    printf '/// ```\n/// ```\302\240\n/// let a = 2;\n/// ```\npub fn r() {}\n' | wr "$T/c23.rs"
    printf '/// ```\n/// ```\034\n/// let a = 2;\n/// ```\npub fn s() {}\n' | wr "$T/c24.rs"
    printf '/// a\rb\npub fn t() {}\n' | wr "$T/c25.rs"
    printf '/** Para.\n\n     * a\n\n     * let a = 2;\n*/\npub fn u() {}\n' | wr "$T/c26.rs"
    printf '/// Para\n///\n#[doc = "      let a = 1;"]\npub fn v() {}\n' | wr "$T/c27.rs"
    printf '//! Module prose, reworded.\npub fn w() {}\n' | wr "$T/src/c28.rs"
    printf '/// Para\n///\nmod mm {\n    //!       let a = 1;\n}\n' | wr "$T/c29.rs"
    printf '/** Para, reworded. */\npub fn x() {}\n' | wr "$T/c30.rs"
    printf '/// Para\n// @generated\n///\n///     let a = 1;\npub fn y() {}\n' | wr "$T/c31.rs"
    printf '/// Para\n///\n/**\n *     let a = 1;\n */\npub fn z() {}\n' | wr "$T/c32.rs"
    printf '/// Para, reworded.\n// note\n/// More.\npub fn aa() {}\n' | wr "$T/c33.rs"
    printf 'macro_rules! mk {\n    () => {\n        /// Made, reworded.\n        pub fn made() {}\n    };\n}\n' | wr "$T/c34.rs"
    printf 'macro_rules! wrap { ($i:item) => { $i }; }\n/// Para, reworded\nwrap! { pub fn ab() {} }\n' | wr "$T/c35.rs"
    printf '#[derive(Debug, clap::Parser)]\npub struct Cli {\n    /// Name of the person to greet.\n    pub name: String,\n}\n' | wr "$T/c36.rs"
    printf '#[derive(Debug, Clone)]\npub struct Plain {\n    /// A field, reworded.\n    #[serde(default)]\n    pub name: String,\n}\n' | wr "$T/c37.rs"
    printf '/// a\014b\npub fn ac() {}\n' | wr "$T/c38.rs"
    printf 'mod m {\n    //! Inner prose, reworded.\n}\n' | wr "$T/c39.rs"
    printf '//! Tool docs, reworded.\n\n/// Main docs, reworded.\nfn main() {}\n' | wr "$T/src/bin/tool.rs"
    printf 'pub const X: u8 = 1;\n// a note, reworded\n' | wr "$T/c41.rs"
    printf '///Prose.\npub fn ad() {\n    //! More.\n    //!\n    //!    assert!(false, "BODY RAN");\n}\n' | wr "$T/c42.rs"
    printf 'pub struct S;\n///Prose.\nimpl S {\n    //! More.\n    //!\n    //!    assert!(false, "IMPL RAN");\n}\n' | wr "$T/c43.rs"
    printf 'pub struct P;\n#[pymethods]\nimpl P {\n    /// Method doc, reworded.\n    pub fn m(&self) {}\n}\n' | wr "$T/c44.rs"
    cm "$T" head
    run_check "$T" --base "$BASE_T" --head HEAD
    expect_file "rust: a // comment edit is non_executable" c01.rs non_executable comments_and_whitespace_only
    expect_file "rust: a /// prose edit is non_executable" c02.rs non_executable comments_and_whitespace_only
    expect_file "rust: a /// edit inside a fenced code block is a doctest" c03.rs executable rust_doc_code_block
    expect_file "rust: a /// edit inside an indented code block is a doctest" c04.rs executable rust_doc_code_block
    expect_file "rust: adding a doc comment is not proven inert" c05.rs executable rust_doc_presence
    expect_file "rust: turning // into /// is not proven inert" c06.rs executable rust_doc_presence
    expect_file "rust: whitespace inside a string literal is executable" c07.rs executable token_change
    expect_file "rust: a raw string's delimiters are compared" c08.rs executable token_change
    expect_file "rust: a parse error on the head side is executable" c09.rs executable parse_error_head
    expect_file "rust: a @generated marker is a tool directive" c10.rs executable directive_comment
    expect_file "rust: a //! inner-doc prose edit in a crate root is non_executable" src/lib.rs non_executable comments_and_whitespace_only
    expect_file "rust: a block comment edit is non_executable" c12.rs non_executable comments_and_whitespace_only
    expect_file "rust: whitespace between tokens is non_executable" c13.rs non_executable comments_and_whitespace_only
    # An indented code block cannot interrupt a paragraph: the indented line is
    # unchanged on both sides, and only the blank line before it moves.
    expect_file "rust: a blank /// line that turns an indented paragraph line into a doctest is executable" c14.rs executable rust_doc_line_indented
    expect_file "rust: removing the blank /// line before an indented doctest is executable" c15.rs executable rust_doc_line_indented
    expect_file "rust: dropping a list marker that holds a fence is executable" c16.rs executable rust_doc_line_block_start
    expect_file "rust: an edit inside a block-quoted fence is executable" c17.rs executable rust_doc_line_block_start
    # CommonMark HTML blocks: the fence's lines are unchanged on both sides.
    expect_file "rust: a blank /// line that ends an HTML block before a fence is executable" c18.rs executable rust_doc_line_block_start
    expect_file "rust: un-commenting an HTML-commented fence is executable" c19.rs executable rust_doc_line_block_start
    # A backtick in a backtick fence's info string makes the line prose, so the
    # FIRST fence is the one after it and `let a = ...;` is the doctest.
    expect_file "rust: a backtick-fence line with a backtick in its info string is not an opener" c20.rs executable rust_doc_code_block
    # rustdoc joins every doc fragment of an item: the fence spans #[inline].
    # Unmerged, the edit would sit in a separately compared group.
    expect_file "rust: an edit inside a fence that spans an attribute is a doctest" c21.rs executable rust_doc_code_block
    # Outer docs on `mod m` and the inner docs inside it are separate groups
    # here; a fence in a changed group is refused rather than modelled.
    expect_file "rust: a doc edit beside an unclosed fence in another group is executable" c22.rs executable rust_doc_line_block_start
    expect_file "rust: a fence closer followed by NBSP is code, not a closer" c23.rs executable rust_doc_code_block
    expect_file "rust: a fence closer followed by \\x1c is code, not a closer" c24.rs executable rust_doc_code_block
    expect_file "rust: a bare CR in a doc comment is a compile error" c25.rs executable rust_doc_bare_cr
    # A blank line without a star turns rustdoc's star strip OFF for the whole
    # comment, so `* let a = ...;` is an indented doctest to rustdoc.
    expect_file "rust: a /** */ doc whose stars rustdoc may strip differently is executable" c26.rs executable
    # The doc attribute's text joins the group: after the added blank line its
    # indented line is a doctest.
    expect_file "rust: a doc group joined with a #[doc = ...] attribute is executable" c27.rs executable rust_doc_joined_attribute
    expect_file "rust: an inner doc edit joined with the parent's docs on mod c28 is executable" src/c28.rs executable rust_doc_joined_parent_module:parent_outer_doc:src/lib.rs
    expect_file "rust: an outer doc edit on a mod item joined with its inner docs is executable" c29.rs executable rust_doc_documents_mod
    # The whitelist: anything but consecutive `///` / `//!` lines of plain
    # prose, outside a macro, on a listed item with inert attributes, is not
    # proven inert.
    expect_file "rust: any /** */ doc change is executable, prose included" c30.rs executable rust_doc_block_comment
    # Round-5 finding 1: rustdoc unindents the JOINED doc, so a fragment split
    # off by a directive comment is not judged alone.
    expect_file "rust: a doc group split by a // @generated comment is one group, refused" c31.rs executable rust_doc_not_contiguous
    # Round-5 finding 2: a /// group joined with a /** */ fragment.
    expect_file "rust: a /// group joined with a /** */ fragment is executable" c32.rs executable rust_doc_block_comment
    expect_file "rust: a comment between two /// fragments is not a consecutive group" c33.rs executable rust_doc_not_contiguous
    expect_file "rust: a doc inside a macro_rules body is executable" c34.rs executable rust_doc_in_macro
    expect_file "rust: a doc on a macro invocation is executable" c35.rs executable rust_doc_item:macro_invocation
    expect_file "rust: a field doc under a doc-reading derive (clap) is executable" c36.rs executable "rust_doc_attribute_reader:derive(clap::Parser)"
    expect_file "rust: a field doc under std derives and serde attributes stays non_executable" c37.rs non_executable comments_and_whitespace_only
    expect_file "rust: a control character in a doc line is executable" c38.rs executable rust_doc_control_char
    expect_file "rust: inner docs inside an inline module are executable" c39.rs executable rust_doc_inner_in_block
    # Negative control: a crate root's //! beside its first item's /// are two
    # items' docs, not one joined group.
    expect_file "rust: prose edits to a bin root's //! and its item's /// stay non_executable" src/bin/tool.rs non_executable comments_and_whitespace_only
    # A source file include_str!'d (here by itself) carries its comments into
    # the program: a comment edit there is not inert.
    expect_file "rust: a comment edit in a file a crate include_str!s is executable" c41.rs executable included_by_rust:c41i.rs
    # Round-6 finding 1: rustdoc joins an item's `///` with the `//!` at the
    # top of its body and unindents them together, so dropping one space in
    # the `///` turns the body's 4-space line into a doctest.
    expect_file "rust: a /// edit on a fn whose body holds //! docs is executable" c42.rs executable rust_doc_joined_body_inner
    expect_file "rust: a /// edit on an impl whose body holds //! docs is executable" c43.rs executable rust_doc_joined_body_inner
    # Clause (vi) on an ENCLOSING item: pyo3's #[pymethods] reads the docs of
    # the methods inside it.
    expect_file "rust: a method doc under an enclosing #[pymethods] impl is executable" c44.rs executable rust_doc_attribute_reader:pymethods
    # rust_doc_whitelist() with no repository trees to read cannot look the
    # parent module up: an inner-doc edit in a module file is UNKNOWN. The
    # driver always passes trees, so this calls the arm directly.
    wr "$SANDBOX/unread.py" <<'PYEOF'
import importlib.util, sys
spec = importlib.util.spec_from_file_location("exeq_unread", sys.argv[1])
m = importlib.util.module_from_spec(spec)
sys.modules["exeq_unread"] = m
spec.loader.exec_module(m)
ts = m.TreeSitter()
ts.load()
c = m.Change(path="src/sub.rs", status="M", old_mode="100644", new_mode="100644", old_sha="1" * 40,
             new_sha="2" * 40, base=b"//! Module prose.\npub fn w() {}\n",
             head=b"//! Module prose, reworded.\npub fn w() {}\n", lang="rust")
m.tree_sitter_arm(c, ts, None)
print("%s %s" % (c.verdict, c.reason))
PYEOF
    UNREAD="$("$PY" "$(native_path_m "$SANDBOX/unread.py")" "$SUBJ_N" 2>&1 | tr -d '\r')"
    if [ "$UNREAD" = "unknown rust_doc_parent_module_unread" ]; then
      ok "rust: an inner-doc edit whose parent module cannot be read is unknown"
    else
      bad "rust: an inner-doc edit whose parent module cannot be read is unknown" "got: ${UNREAD:0:600}"
    fi

    # Round 33 F3: a Rust file can name its own path.
    K="$SANDBOX/r33rs"; mkrepo "$K"
    printf '// AAA\nfn main() { println!("{}", std::fs::read_to_string(file!()).unwrap()); }\n' | wr "$K/r1.rs"
    printf '// AAA\nconst D: &str = env!("CARGO_MANIFEST_DIR");\n' | wr "$K/r2.rs"
    printf '// AAA\nfn main() { let _ = std::fs::read_to_string("src/r3.rs"); }\n' | wr "$K/r3.rs"
    cm "$K" base; BASE_K="$(git -C "$K" rev-parse HEAD)"
    for f in r1 r2 r3; do sed 's/AAA/BBB/' "$K/$f.rs" > "$K/$f.tmp" && mv "$K/$f.tmp" "$K/$f.rs"; done
    cm "$K" head
    run_check "$K" --base "$BASE_K" --head HEAD
    expect_file "rust: r33 F3: file!() inside another macro names the file" r1.rs executable source_reader:self_named:file!
    expect_file "rust: r33 F3: CARGO_MANIFEST_DIR names the file's tree" r2.rs executable source_reader:self_named:CARGO_MANIFEST_DIR
    expect_file "rust: r33 F3: a literal naming the file names it" r3.rs executable source_reader:self_named:literal:r3.rs
  else
    ts_unmeasured rust
  fi
fi

if mc_sec ts-core; then
  echo "== ts-core"
  if [ "$TS_OK" = 1 ]; then
    T="$SANDBOX/ts"; mkrepo "$T"
    printf '/**\n * Adds.\n * @param a first\n */\nexport function add(a: number): number { return a; }\n' | wr "$T/t01.ts"
    printf 'export const q = "q";\n' | wr "$T/t02.ts"
    printf 'export const n: number = 1;\n' | wr "$T/t03.ts"
    printf 'export function f(v: number) {\n  return /* c */ v;\n}\n' | wr "$T/t04.ts"
    printf 'export const x = <div>{/* old */}hi</div>;\n' | wr "$T/t05.tsx"
    printf '/** @type {number} */\nexport const t = 1;\n' | wr "$T/t06.js"
    printf '/**\n * Old prose.\n * @param {number} a the value\n */\nexport function g(a) { return a; }\n' | wr "$T/t07.js"
    printf '/// <reference types="node" />\nexport {};\n' | wr "$T/t08.ts"
    printf 'export const z = make();\n' | wr "$T/t09.ts"
    printf 'export const s = `a ${v /* x */} b`;\n' | wr "$T/t10.ts"
    printf 'export const r = /ab+c/;\n' | wr "$T/t11.ts"
    printf 'export const k = 1 /* a */ + 2;\n' | wr "$T/t12.ts"
    printf 'export const a = b /* c */ (d);\n' | wr "$T/t13.ts"
    cm "$T" base; BASE_T="$(git -C "$T" rev-parse HEAD)"
    printf '/**\n * Adds two numbers.\n * @param a first\n */\nexport function add(a: number): number { return a; }\n' | wr "$T/t01.ts"
    printf 'export const q = "r";\n' | wr "$T/t02.ts"
    printf '// @ts-expect-error\nexport const n: number = 1;\n' | wr "$T/t03.ts"
    printf 'export function f(v: number) {\n  return // c\n  v;\n}\n' | wr "$T/t04.ts"
    printf 'export const x = <div>{/* new */}hi</div>;\n' | wr "$T/t05.tsx"
    printf '/** @type {string} */\nexport const t = 1;\n' | wr "$T/t06.js"
    printf '/**\n * New prose.\n * @param {number} a the value\n */\nexport function g(a) { return a; }\n' | wr "$T/t07.js"
    printf '/// <reference types="bun" />\nexport {};\n' | wr "$T/t08.ts"
    printf 'export const z = /*#__PURE__*/ make();\n' | wr "$T/t09.ts"
    printf 'export const s = `a ${v /* y */} b`;\n' | wr "$T/t10.ts"
    printf 'export const r = /ab+d/;\n' | wr "$T/t11.ts"
    printf 'export const k = 1 /* b */ + 2;\n' | wr "$T/t12.ts"
    printf 'export const a = b // c\n(d);\n' | wr "$T/t13.ts"
    cm "$T" head
    run_check "$T" --base "$BASE_T" --head HEAD
    expect_file "ts: a JSDoc prose edit is non_executable" t01.ts non_executable comments_and_whitespace_only
    expect_file "ts: a string literal change is executable" t02.ts executable token_change
    expect_file "ts: an added @ts-expect-error is a tool directive" t03.ts executable directive_comment
    expect_file "ts: a comment that puts a line break after return is executable (ASI)" t04.ts executable
    expect_file "ts: a JSX comment edit is non_executable" t05.tsx non_executable comments_and_whitespace_only
    expect_file "ts: a JSDoc @type change in a .js file is a directive" t06.js executable directive_comment
    expect_file "ts: a JSDoc prose edit beside an unchanged type tag is non_executable" t07.js non_executable comments_and_whitespace_only
    expect_file "ts: a triple-slash directive change is executable" t08.ts executable directive_comment
    expect_file "ts: an added /*#__PURE__*/ annotation is executable" t09.ts executable directive_comment
    expect_file "ts: a literal is compared whole, a comment inside it included" t10.ts executable token_change
    expect_file "ts: a regex literal change is executable" t11.ts executable token_change
    expect_file "ts: a same-line block comment edit is non_executable" t12.ts non_executable comments_and_whitespace_only
    # Conservative by design: here no semicolon is inserted either way, but a
    # line break a comment adds or removes between two tokens is compared in
    # case the grammar's ASI ever disagrees with a runtime's.
    expect_file "ts: a comment edit that adds a line break between two tokens is executable" t13.ts executable line_break_between_tokens
  else
    ts_unmeasured ts-core
  fi
fi

if mc_sec ts-self; then
  echo "== ts-self"
  if [ "$TS_OK" = 1 ]; then
    # Round 33 F3: Function.prototype.toString returns a function's or class's
    # text, comments and whitespace included, and a module can name its own
    # path.
    J="$SANDBOX/r33js"; mkrepo "$J"
    printf 'const f = function () { /* AAA */ };\nconsole.log(f.toString());\n' | wr "$J/j1.js"
    printf 'const f = () => 1;\nconsole.log("" + f);\n' | wr "$J/j2.js"
    printf '// AAA\nexport const v = 1;\n' | wr "$J/j3.js"
    printf '// AAA\nconsole.log(require("fs").readFileSync(__filename, "utf8"));\n' | wr "$J/j4.js"
    printf '// AAA\nconsole.log(new URL(import.meta.url).pathname);\n' | wr "$J/j5.mjs"
    printf '// AAA\nexport const p = "./j6.js";\n' | wr "$J/j6.ts"
    printf 'class C { /* AAA */ m() {} }\nconsole.log(`${C}`);\n' | wr "$J/j7.js"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    printf 'const f = () =>  1;\nconsole.log("" + f);\n' | wr "$J/j2.js"
    for f in j1.js j3.js j4.js j5.mjs j6.ts j7.js; do
      sed 's/AAA/BBB/' "$J/$f" > "$J/$f.tmp" && mv "$J/$f.tmp" "$J/$f"
    done
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r33 F3: a comment inside a function is in its toString text" j1.js executable source_reader:js_function_source_text
    expect_file "ts: r33 F3: whitespace inside an arrow function is in its string form" j2.js executable source_reader:js_function_source_text
    expect_file "ts: r33: a top-level comment edit in a module that never names itself stays non_executable" j3.js non_executable comments_and_whitespace_only
    expect_file "ts: r33 F3: __filename names the module" j4.js executable source_reader:self_named:__filename
    expect_file "ts: r33 F3: import.meta names the module" j5.mjs executable source_reader:self_named:import.meta
    expect_file "ts: r33 F3: a literal naming the emitted file names the module" j6.ts executable source_reader:self_named:literal:j6.js
    expect_file "ts: r33 F3: a comment in a class body is in the class's string form" j7.js executable source_reader:js_function_source_text
    # Round 34 F3: a dotted name and an extensionless or directory specifier
    # name the module; F4: whatever reaches the CommonJS wrapper reads its
    # whole text.
    J="$SANDBOX/r34js"; mkrepo "$J"
    printf '// AAA\nexport const p = "./k1.test.js";\n' | wr "$J/k1.test.ts"
    printf '// AAA\nconsole.log(require("fs").readFileSync(require.resolve("./k2"), "utf8"));\n' | wr "$J/k2.js"
    printf '// AAA\nconst p = require.resolve("./k3");\n' | wr "$J/k3/index.js"
    printf '// AAA\nconsole.log(arguments.callee.toString());\n' | wr "$J/k4.js"
    printf '// AAA\nfunction f() { return f.caller; }\nconsole.log(String(f()));\n' | wr "$J/k5.js"
    printf '// AAA\nconsole.log(String(arguments[1].main));\n' | wr "$J/k6.js"
    printf '// AAA\nconsole.log(Object.keys(require.cache));\n' | wr "$J/k7.js"
    printf '// AAA\nfunction f() { return arguments.length; }\nconst x = "k";\n' | wr "$J/k8.js"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    for f in k1.test.ts k2.js k3/index.js k4.js k5.js k6.js k7.js k8.js; do
      sed 's/AAA/BBB/' "$J/$f" > "$J/$f.tmp" && mv "$J/$f.tmp" "$J/$f"
    done
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r34 F3: a dotted name is stemmed at its last extension" k1.test.ts executable source_reader:self_named:literal:k1.test.js
    expect_file "ts: r34 F3: an extensionless specifier names the module" k2.js executable source_reader:self_named:specifier:./k2
    expect_file "ts: r34 F3: a directory specifier names its index module" k3/index.js executable source_reader:self_named:specifier:./k3
    expect_file "ts: r34 F4: arguments.callee reads the CommonJS wrapper" k4.js executable source_reader:self_named:callee
    expect_file "ts: r34 F4: a top-level function's caller is the CommonJS wrapper" k5.js executable source_reader:self_named:caller
    expect_file "ts: r34 F4: the module-level arguments are the CommonJS wrapper's" k6.js executable source_reader:self_named:arguments
    expect_file "ts: r34 F4: require.cache holds the module's path" k7.js executable source_reader:self_named:require.cache
    expect_file "ts: r34: a function's own arguments and an unrelated string stay non_executable" k8.js non_executable comments_and_whitespace_only
    # Round 35 F3: a self-name spelled with brackets or destructured, and a
    # computed property of a self-naming object.
    J="$SANDBOX/r35js"; mkrepo "$J"
    printf '// AAA\nfunction f() { return f["caller"]; }\nconsole.log(String(f()));\n' | wr "$J/m1.js"
    printf '// AAA\nconsole.log(module["filename"]);\n' | wr "$J/m2.js"
    printf '// AAA\nconst {filename} = module;\nconsole.log(filename);\n' | wr "$J/m3.js"
    printf '// AAA\nconst k = "filename";\nconsole.log(module[k]);\n' | wr "$J/m4.js"
    printf '// AAA\nfunction f() { const {["caller"]: c} = f; return c; }\n' | wr "$J/m5.js"
    printf '// AAA\nconst o = {x: 1};\nconst {x} = o;\nconsole.log(o["x"], o[x]);\n' | wr "$J/m6.js"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    for f in m1 m2 m3 m4 m5 m6; do
      sed 's/AAA/BBB/' "$J/$f.js" > "$J/$f.tmp" && mv "$J/$f.tmp" "$J/$f.js"
    done
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r35 F3: f[\"caller\"] reaches the CommonJS wrapper" m1.js executable source_reader:self_named:caller
    expect_file "ts: r35 F3: module[\"filename\"] names the module" m2.js executable source_reader:self_named:module.filename
    expect_file "ts: r35 F3: a destructured filename names the module" m3.js executable source_reader:self_named:module.filename
    expect_file "ts: r35 F3: a computed property of module may name the module" m4.js executable source_reader:self_named:computed:module
    expect_file "ts: r35 F3: a computed-key destructured caller reaches the wrapper" m5.js executable source_reader:self_named:caller
    expect_file "ts: r35: bracket and destructured reads of an ordinary object stay non_executable" m6.js non_executable comments_and_whitespace_only
    # Round 36: a key or literal is read by its decoded value, and an object
    # through what does not change it.
    J="$SANDBOX/r36js"; mkrepo "$J"
    printf '// AAA\nfunction f() { return f["\\u0063aller"]; }\nconsole.log(String(f()));\n' | wr "$J/n1.js"
    printf '// AAA\nfunction f() { return f[`caller`]; }\nconsole.log(String(f()));\n' | wr "$J/n2.js"
    printf '// AAA\nconsole.log((module).filename);\n' | wr "$J/n3.js"
    printf '// AAA\nconsole.log((0, module).filename);\n' | wr "$J/n4.js"
    printf '// AAA\nconsole.log(globalThis.process.argv);\n' | wr "$J/n5.js"
    printf '// AAA\nconsole.log((module as any).filename);\n' | wr "$J/n6.ts"
    printf '// AAA\nconst p = "./n7\\x2ejs";\n' | wr "$J/n7.js"
    printf '// AAA\nconst o = {}; const x = 1;\nconsole.log(o[`a${x}`], o["\\x61"]);\n' | wr "$J/n8.js"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    for f in n1.js n2.js n3.js n4.js n5.js n6.ts n7.js n8.js; do
      sed 's/AAA/BBB/' "$J/$f" > "$J/$f.tmp" && mv "$J/$f.tmp" "$J/$f"
    done
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r36: an escaped key is read decoded" n1.js executable source_reader:self_named:caller
    expect_file "ts: r36: a plain template key is a static name" n2.js executable source_reader:self_named:caller
    expect_file "ts: r36: a parenthesised module is module" n3.js executable source_reader:self_named:module.filename
    expect_file "ts: r36: a comma operator's last operand is the object" n4.js executable source_reader:self_named:module.filename
    expect_file "ts: r36: globalThis.process is process" n5.js executable source_reader:self_named:process.argv
    expect_file "ts: r36: a TS as-assertion does not hide module" n6.ts executable source_reader:self_named:module.filename
    expect_file "ts: r36: an escaped literal naming the file names the module" n7.js executable source_reader:self_named:literal:n7.js
    expect_file "ts: r36: computed and escaped keys of an ordinary object stay non_executable" n8.js non_executable comments_and_whitespace_only
  else
    ts_unmeasured ts-self
  fi
fi

if mc_sec ts-escape; then
  echo "== ts-escape"
  if [ "$TS_OK" = 1 ]; then
    # Round 37: legacy octal escapes are decoded.
    J="$SANDBOX/r37js"; mkrepo "$J"
    printf '// AAA\nfunction f() { return f["\\143aller"]; }\nconsole.log(String(f()));\n' | wr "$J/p1.js"
    printf '// AAA\nconst p = "./p2\\56js";\n' | wr "$J/p2.js"
    printf '// AAA\nconst z = "a\\0b\\101";\n' | wr "$J/p3.js"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    for f in p1 p2 p3; do
      sed 's/AAA/BBB/' "$J/$f.js" > "$J/$f.tmp" && mv "$J/$f.tmp" "$J/$f.js"
    done
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r37: a legacy octal escape in a key is decoded" p1.js executable source_reader:self_named:caller
    expect_file "ts: r37: a legacy octal escape in a literal naming the file is decoded" p2.js executable source_reader:self_named:literal:p2.js
    expect_file "ts: r37: a string with octal escapes naming nothing stays non_executable" p3.js non_executable comments_and_whitespace_only
    # Round 38: after a 4-7, JS reads at most two octal digits.
    J="$SANDBOX/r38js"; mkrepo "$J"
    printf '// AAA\nconsole.log(require("fs").readFileSync("q\\600.js", "utf8"));\n' | wr "$J/q00.js"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    sed 's/AAA/BBB/' "$J/q00.js" > "$J/q00.tmp" && mv "$J/q00.tmp" "$J/q00.js"
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r38: a three-digit octal escape after a 4-7 decodes two digits" q00.js executable source_reader:self_named:literal:q00.js
    # Round 39: JS strings are UTF-16, so a \u surrogate pair is one code point.
    J="$SANDBOX/r39js"; mkrepo "$J"
    printf '// AAA\nconsole.log(require("fs").readFileSync("s\\uD83D\\uDE00.js", "utf8"));\n' | wr "$J/s😀.js"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    sed 's/AAA/BBB/' "$J/s😀.js" > "$J/s.tmp" && mv "$J/s.tmp" "$J/s😀.js"
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r39: a \\u surrogate pair in a literal naming the file is one code point" "s😀.js" executable "source_reader:self_named:literal:s😀.js"
    # Round 40: node writes a lone surrogate, and a source byte that is not
    # UTF-8, as U+FFFD; a raw byte never pairs with a \u surrogate.
    J="$SANDBOX/r40js"; mkrepo "$J"
    printf '// AAA\nconsole.log(require("fs").readFileSync("t\\uD83D.js", "utf8"));\n' | wr "$J/t�.js"
    printf '// AAA\nconsole.log(require("fs").readFileSync("u\377.js", "utf8"));\n' | wr "$J/u�.js"
    printf '// AAA\nconsole.log(require("fs").readFileSync("v\\uD83D\377.js", "utf8"));\n' | wr "$J/v��.js"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    for f in "t�.js" "u�.js" "v��.js"; do
      sed 's/AAA/BBB/' "$J/$f" > "$J/x.tmp" && mv "$J/x.tmp" "$J/$f"
    done
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r40: a lone \\u surrogate is U+FFFD in a path" "t�.js" executable "source_reader:self_named:literal:t�.js"
    expect_file "ts: r40: a source byte that is not UTF-8 is U+FFFD" "u�.js" executable "source_reader:self_named:literal:u�.js"
    expect_file "ts: r40: a raw byte does not pair with a \\u surrogate" "v��.js" executable "source_reader:self_named:literal:v��.js"
    # Round 41: an identifier is read with its \u escapes decoded, and a
    # template reads a CR as LF.
    J="$SANDBOX/r41js"; mkrepo "$J"
    printf '// AAA\nconsole.log(require("fs").readFileSync(__file\\u006eame, "utf8"));\n' | wr "$J/w1.js"
    printf '// AAA\nconsole.log(require("fs").readFileSync(module.file\\u006eame, "utf8"));\n' | wr "$J/w2.js"
    printf '// AAA\nconsole.log(require("fs").readFileSync(process.\\u0061rgv[1], "utf8"));\n' | wr "$J/w3.js"
    printf '// AAA\nconsole.log(require("fs").readFileSync(`w\r4.js`, "utf8"));\n' | wr "$J/w"$'\n'"4.js"
    printf '// AAA\nconst __file\\u0078 = 1;\nconsole.log(__file\\u0078);\n' | wr "$J/w5.js"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    for f in w1.js w2.js w3.js "w"$'\n'"4.js" w5.js; do
      sed 's/AAA/BBB/' "$J/$f" > "$J/x.tmp" && mv "$J/x.tmp" "$J/$f"
    done
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r41: an escaped __filename is __filename" w1.js executable source_reader:self_named:__filename
    expect_file "ts: r41: an escaped property name is read decoded" w2.js executable source_reader:self_named:module.filename
    expect_file "ts: r41: an escaped process.argv is process.argv" w3.js executable source_reader:self_named:process.argv
    expect_file "ts: r41: a template reads a CR as LF" 'w\\n4.js' executable 'source_reader:self_named:literal:w'
    expect_file "ts: r41: an escaped identifier naming nothing stays non_executable" w5.js non_executable comments_and_whitespace_only
  else
    ts_unmeasured ts-escape
  fi
fi

if mc_sec ts-jsx; then
  echo "== ts-jsx"
  if [ "$TS_OK" = 1 ]; then
    # Round 42: a module-level arguments written as an object shorthand, and
    # JSX strings and text read with their character references decoded.
    J="$SANDBOX/r42js"; mkrepo "$J"
    printf '// AAA\nconsole.log(require("fs").readFileSync({arguments}.arguments[3], "utf8"));\n' | wr "$J/x1.js"
    printf '// AAA\nconst o = {argument\\u0073};\nconsole.log(o);\n' | wr "$J/x2.js"
    printf '// AAA\nfunction h(t, p) { return p; }\nconsole.log(require("fs").readFileSync(<a b="x3&#46;jsx" />.b, "utf8"));\n' | wr "$J/x3.jsx"
    printf '// AAA\nconst e = <p>x4&#46;jsx</p>;\n' | wr "$J/x4.jsx"
    printf '// AAA\nconst e = <a b="k&amp;v">t&amp;u</a>;\n' | wr "$J/x5.jsx"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    for f in x1.js x2.js x3.jsx x4.jsx x5.jsx; do
      sed 's/AAA/BBB/' "$J/$f" > "$J/x.tmp" && mv "$J/x.tmp" "$J/$f"
    done
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r42: a shorthand {arguments} is the CommonJS wrapper's" x1.js executable source_reader:self_named:arguments
    expect_file "ts: r42: an escaped shorthand {argument\\u0073} is too" x2.js executable source_reader:self_named:arguments
    expect_file "ts: r42: a JSX attribute string decodes character references" x3.jsx executable source_reader:self_named:literal:x3.js
    expect_file "ts: r42: JSX text decodes character references" x4.jsx executable source_reader:self_named:literal:x4.js
    expect_file "ts: r42: JSX strings and text naming nothing stay non_executable" x5.jsx non_executable comments_and_whitespace_only
    # Round 43: JSX decodes character references by the XHTML table, numeric
    # ones straight to their code point, and trims and joins JSX text lines.
    J="$SANDBOX/r43jsx"; mkrepo "$J"
    printf '// AAA\nfunction h(t, p) { return p; }\nconsole.log(require("fs").readFileSync(<a b="l&lang;.jsx" />.b, "utf8"));\n' | wr "$J/l"$'〈'".jsx"
    printf '// AAA\nfunction h(t, p, c) { return c; }\nconsole.log(require("fs").readFileSync(<p>s\n   p.jsx</p>, "utf8"));\n' | wr "$J/s p.jsx"
    printf '// AAA\nfunction h(t, p) { return p; }\nconsole.log(require("fs").readFileSync(<a b="c&#x80;.jsx" />.b, "utf8"));\n' | wr "$J/c"$'\u0080'".jsx"
    printf '// AAA\nconst e = <a b="k&lang;&#x80;v">t\n   u</a>;\n' | wr "$J/k.jsx"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    for f in "l"$'〈'".jsx" "s p.jsx" "c"$'\u0080'".jsx" k.jsx; do
      sed 's/AAA/BBB/' "$J/$f" > "$J/x.tmp" && mv "$J/x.tmp" "$J/$f"
    done
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r43: a named JSX reference decodes by the XHTML table" "l"$'〈'".jsx" executable "source_reader:self_named:literal:l"$'〈'".js"
    expect_file "ts: r43: multi-line JSX text is trimmed and joined" "s p.jsx" executable "source_reader:self_named:literal:s p.js"
    expect_file "ts: r43: a numeric JSX reference is its code point, unremapped" "c"$'\u0080'".jsx" executable "source_reader:self_named:literal:c"$'\u0080'".js"
    expect_file "ts: r43: JSX references and text naming nothing stay non_executable" k.jsx non_executable comments_and_whitespace_only
    # Round 44: tsc and esbuild also split JSX text at U+2028/U+2029 and trim
    # JS whitespace (a no-break space) at a line's edges.
    J="$SANDBOX/r44jsx"; mkrepo "$J"
    printf '// AAA\nfunction h(t, p, c) { return c; }\nconsole.log(require("fs").readFileSync(<p>s\342\200\250   p.jsx</p>, "utf8"));\n' | wr "$J/s p.jsx"
    printf '// AAA\nfunction h(t, p, c) { return c; }\nconsole.log(require("fs").readFileSync(<p>q\302\240\n   r.jsx</p>, "utf8"));\n' | wr "$J/q r.jsx"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    for f in "s p.jsx" "q r.jsx"; do
      sed 's/AAA/BBB/' "$J/$f" > "$J/x.tmp" && mv "$J/x.tmp" "$J/$f"
    done
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r44: JSX text splits at U+2028 as tsc and esbuild do" "s p.jsx" executable "source_reader:self_named:literal:s p.js"
    expect_file "ts: r44: JSX text trims a no-break space as tsc and esbuild do" "q r.jsx" executable "source_reader:self_named:literal:q r.js"
    # Round 45: Babel decodes references before it trims and turns every tab
    # into a space; tsc also trims NEL.
    J="$SANDBOX/r45jsx"; mkrepo "$J"
    printf '// AAA\nfunction h(t, p, c) { return c; }\nconsole.log(require("fs").readFileSync(<p>s&#10;   p.jsx</p>, "utf8"));\n' | wr "$J/s p.jsx"
    printf '// AAA\nfunction h(t, p, c) { return c; }\nconsole.log(require("fs").readFileSync(<p>q\302\205\n   r.jsx</p>, "utf8"));\n' | wr "$J/q r.jsx"
    printf '// AAA\nfunction h(t, p, c) { return c; }\nconsole.log(require("fs").readFileSync(<p>t\tu.jsx</p>, "utf8"));\n' | wr "$J/t u.jsx"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    for f in "s p.jsx" "q r.jsx" "t u.jsx"; do
      sed 's/AAA/BBB/' "$J/$f" > "$J/x.tmp" && mv "$J/x.tmp" "$J/$f"
    done
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r45: Babel decodes a reference before it trims JSX text" "s p.jsx" executable "source_reader:self_named:literal:s p.js"
    expect_file "ts: r45: tsc trims NEL at a JSX text line edge" "q r.jsx" executable "source_reader:self_named:literal:q r.js"
    expect_file "ts: r45: Babel turns every tab in JSX text into a space" "t u.jsx" executable "source_reader:self_named:literal:t u.js"
    # Round 46: no JSX compiler decodes an uppercase &#X..; reference.
    J="$SANDBOX/r46jsx"; mkrepo "$J"
    printf '// AAA\nfunction h(t, p, c) { return c; }\nconsole.log(require("fs").readFileSync(<p>x&#X41;.jsx</p>, "utf8"));\n' | wr "$J/x&#X41;.jsx"
    printf '// AAA\nfunction h(t, p) { return p; }\nconsole.log(require("fs").readFileSync(<a b="y&#X41;.jsx" />.b, "utf8"));\n' | wr "$J/y&#X41;.jsx"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    for f in "x&#X41;.jsx" "y&#X41;.jsx"; do
      sed 's/AAA/BBB/' "$J/$f" > "$J/x.tmp" && mv "$J/x.tmp" "$J/$f"
    done
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r46: an uppercase &#X..; in JSX text stays as written" "x&#X41;.jsx" executable "source_reader:self_named:literal:x&#X41;.js"
    expect_file "ts: r46: an uppercase &#X..; in a JSX attribute string stays as written" "y&#X41;.jsx" executable "source_reader:self_named:literal:y&#X41;.js"
    # Round 47: a lone surrogate in JSX text is U+FFFD, and a numeric reference
    # past U+10FFFF (esbuild decodes it, tsc and Babel refuse) fails closed.
    J="$SANDBOX/r47jsx"; mkrepo "$J"
    printf '// AAA\nfunction h(t, p, c) { return c; }\nconsole.log(require("fs").readFileSync(<p>x&#xD800;.jsx</p>, "utf8"));\n' | wr "$J/x"$'�'".jsx"
    printf '// AAA\nconst e = <p>z&#x110000;</p>;\n' | wr "$J/z.jsx"
    printf '// AAA\nconst e = <p>q&#x10FFFF;</p>;\n' | wr "$J/q.jsx"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    for f in "x"$'�'".jsx" z.jsx q.jsx; do
      sed 's/AAA/BBB/' "$J/$f" > "$J/x.tmp" && mv "$J/x.tmp" "$J/$f"
    done
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r47: a lone surrogate in JSX text is U+FFFD" "x"$'�'".jsx" executable "source_reader:self_named:literal:x"$'�'".js"
    expect_file "ts: r47: a JSX reference past U+10FFFF fails closed" z.jsx executable source_reader:self_named:jsx_reference_out_of_range
    expect_file "ts: r47: a JSX reference at U+10FFFF naming nothing stays non_executable" q.jsx non_executable comments_and_whitespace_only
  else
    ts_unmeasured ts-jsx
  fi
fi

if mc_sec ts-jsx2; then
  echo "== ts-jsx2"
  if [ "$TS_OK" = 1 ]; then
    # Round 48: a JSX text run keeps the gap after a reference, and a reference
    # node that carries the blank before it is still checked for range.
    J="$SANDBOX/r48jsx"; mkrepo "$J"
    printf '// AAA\nfunction h(t, p, c) { return c; }\nconsole.log(require("fs").readFileSync(<p>&#98;\n&#46;jsx</p>, "utf8"));\n' | wr "$J/b .jsx"
    printf '// AAA\nconst e = <a b="&#97; &#x110000;.js" />;\n' | wr "$J/d.jsx"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    for f in "b .jsx" d.jsx; do
      sed 's/AAA/BBB/' "$J/$f" > "$J/x.tmp" && mv "$J/x.tmp" "$J/$f"
    done
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r48: the gap after a JSX reference is kept" "b .jsx" executable "source_reader:self_named:literal:b .js"
    expect_file "ts: r48: a JSX reference with a blank before it is range-checked" d.jsx executable source_reader:self_named:jsx_reference_out_of_range
    # Round 49: whitespace before a run's first reference is kept too (Babel
    # trims only spaces and tabs, so it keeps a U+2028 or a VT there).
    J="$SANDBOX/r49jsx"; mkrepo "$J"
    printf '// AAA\nfunction h(t, p, c) { return c; }\nconsole.log(require("fs").readFileSync(<p>\n\342\200\250&#98;.jsx</p>, "utf8"));\n' | wr "$J/"$' '"b.jsx"
    printf '// AAA\nfunction h(t, p, c) { return c; }\nconsole.log(require("fs").readFileSync(<p>\n\v&#100;.jsx</p>, "utf8"));\n' | wr "$J/"$'\v'"d.jsx"
    printf '// AAA\nfunction h(t, p, c) { return c; }\nconsole.log(require("fs").readFileSync(<p><i/>\n\342\200\250&#101;.jsx</p>, "utf8"));\n' | wr "$J/"$' '"e.jsx"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    for f in $' '"b.jsx" $'\v'"d.jsx" $' '"e.jsx"; do
      sed 's/AAA/BBB/' "$J/$f" > "$J/x.tmp" && mv "$J/x.tmp" "$J/$f"
    done
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r49: a U+2028 before a run's first reference is kept" $' '"b.jsx" executable "source_reader:self_named:literal:"$' '"b.js"
    expect_file "ts: r49: a VT before a run's first reference is kept" '\vd.jsx' executable 'source_reader:self_named:literal:'
    expect_file "ts: r49: whitespace after a child element and before a reference is kept" $' '"e.jsx" executable "source_reader:self_named:literal:"$' '"e.js"
    # Round 50: Babel collapses an LF and the JS whitespace after it in a JSX
    # attribute string into one space; tsc and esbuild keep it as written.
    J="$SANDBOX/r50jsx"; mkrepo "$J"
    printf '// AAA\nfunction h(t, p) { return p.c; }\nconsole.log(require("fs").readFileSync(<p c="q\n   r.jsx"/>, "utf8"));\n' | wr "$J/q r.jsx"
    printf '// AAA\nconst e = <p c="k\n   v"/>;\n' | wr "$J/k.jsx"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    for f in "q r.jsx" k.jsx; do
      sed 's/AAA/BBB/' "$J/$f" > "$J/x.tmp" && mv "$J/x.tmp" "$J/$f"
    done
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r50: a JSX attribute string read as Babel collapses it" "q r.jsx" executable "source_reader:self_named:literal:q r.js"
    expect_file "ts: r50: a multi-line attribute string naming nothing stays non_executable" k.jsx non_executable comments_and_whitespace_only
    # Round 51: each reading of a JSX string is tested on its own, so the
    # anchored specifier test still sees `.` and `./` in an index module.
    J="$SANDBOX/r51jsx"; mkrepo "$J"
    printf '// AAA\nfunction h(t, p) { return p.c; }\nconsole.log(require.resolve(<p c="."/>));\n' | wr "$J/d/index.jsx"
    printf '// AAA\nfunction h(t, p, c) { return c; }\nconsole.log(require.resolve(<p>./</p>));\n' | wr "$J/f/index.jsx"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    for f in d/index.jsx f/index.jsx; do
      sed 's/AAA/BBB/' "$J/$f" > "$J/x.tmp" && mv "$J/x.tmp" "$J/$f"
    done
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r51: a JSX attribute string . names its index module" d/index.jsx executable "source_reader:self_named:specifier:."
    expect_file "ts: r51: JSX text ./ names its index module" f/index.jsx executable "source_reader:self_named:specifier:./"
    # Round 52: a JSX attribute string is read whole, even where tree-sitter
    # gives it a `comment` child (a value starting `//` or `/*` with a
    # character reference in it).
    J="$SANDBOX/r52jsx"; mkrepo "$J"
    printf '// AAA\nfunction h(t, p) { return p.c; }\nconsole.log(<p c="//&amp; r.jsx\n"/>);\n' | wr "$J/r.jsx"
    printf '// AAA\nfunction h(t, p) { return p.c; }\nconsole.log(require.resolve(<p c="/*&amp;*/."/>));\n' | wr "$J/d/index.jsx"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    for f in r.jsx d/index.jsx; do
      sed 's/AAA/BBB/' "$J/$f" > "$J/x.tmp" && mv "$J/x.tmp" "$J/$f"
    done
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r52: a JSX attribute string with a comment-shaped child is read whole" r.jsx executable source_reader:self_named:literal:r.js
    expect_file "ts: r52: so is its specifier in an index module" d/index.jsx executable source_reader:self_named:specifier:
    # Round 53: a VT or FF in whitespace-only JSX text (a gap tree-sitter puts
    # in no node) is a text child to Babel, so it is not blank.
    J="$SANDBOX/r53jsx"; mkrepo "$J"
    printf 'const e = <p>\n</p>;\n' | wr "$J/v.jsx"
    printf 'const e = <p><b/>\n</p>;\n' | wr "$J/f.jsx"
    printf 'const e = <p>\n  </p>;\n' | wr "$J/c.jsx"
    cm "$J" base; BASE_J="$(git -C "$J" rev-parse HEAD)"
    printf 'const e = <p>\n\v</p>;\n' | wr "$J/v.jsx"
    printf 'const e = <p><b/>\n\f</p>;\n' | wr "$J/f.jsx"
    printf 'const e = <p>\n\t\t</p>;\n' | wr "$J/c.jsx"
    cm "$J" head
    run_check "$J" --base "$BASE_J" --head HEAD
    expect_file "ts: r53: a VT added to whitespace-only JSX text is a change" v.jsx executable token_change
    expect_file "ts: r53: an FF after a child element is a change" f.jsx executable token_change
    expect_file "ts: r53: spaces and tabs in whitespace-only JSX text stay non_executable" c.jsx non_executable comments_and_whitespace_only
  else
    ts_unmeasured ts-jsx2
  fi
fi

if mc_sec shell-core; then
  echo "== shell-core"
  if [ "$TS_OK" = 1 ]; then
    T="$SANDBOX/shell"; mkrepo "$T"
    printf '#!/bin/bash\n# old\necho hi\n' | wr "$T/s01.sh"
    printf '#!/bin/bash\necho hi\n' | wr "$T/s02.sh"
    printf '#!/bin/bash\n# shellcheck disable=SC2086\necho $x\n' | wr "$T/s03.sh"
    printf '#!/bin/bash\n# usage: s04.sh\nsed -n 2p "$0"\n' | wr "$T/s04.sh"
    printf "#!/bin/bash\n# note\nawk '{print \$0}' f\n" | wr "$T/s05.sh"
    printf '#!/bin/bash\necho "a b"\n' | wr "$T/s06.sh"
    printf '#!/bin/bash\nhead -3 "${BASH_SOURCE[0]}"\n# c\n' | wr "$T/s07.sh"
    printf '#!/usr/bin/env bash\n# old\necho t\n' | wr "$T/bin/tool"
    printf '#!/bin/bash\n# usage: s08.sh\nSELF="$0"\nsed -n 2p "$SELF"\n' | wr "$T/s08.sh"
    printf '#!/bin/bash\n# c\nSELF="${BASH_SOURCE[0]}"\nwhile read -r l; do echo "$l"; done < "$SELF"\n' | wr "$T/s09.sh"
    printf '#!/bin/bash\n# c\necho "usage: $0"\ngrep -c x f\n' | wr "$T/s10.sh"
    printf '#!/bin/bash\n# c\nexec 3<"$0"\nwhile read -r -u3 l; do echo "$l"; done\n' | wr "$T/s11.sh"
    printf '#!/bin/bash\n# c\ncommand sed -n 2p "$0"\n' | wr "$T/s12.sh"
    printf '#!/bin/bash\n# c\nSELF="$0"\nexec 4<"$SELF"\nhead -n 2 <&4\n' | wr "$T/s13.sh"
    printf '#!/bin/bash\n# c\nSELF="${BASH_SOURCE[0]}"\ntimeout 5 env LC_ALL=C sed -n 2p "$SELF"\n' | wr "$T/s14.sh"
    cm "$T" base; BASE_T="$(git -C "$T" rev-parse HEAD)"
    printf '#!/bin/bash\n# new\necho hi\n' | wr "$T/s01.sh"
    printf '#!/bin/sh\necho hi\n' | wr "$T/s02.sh"
    printf '#!/bin/bash\n# shellcheck disable=SC2046\necho $x\n' | wr "$T/s03.sh"
    printf '#!/bin/bash\n# usage: s04.sh [-h]\nsed -n 2p "$0"\n' | wr "$T/s04.sh"
    printf "#!/bin/bash\n# note, changed\nawk '{print \$0}' f\n" | wr "$T/s05.sh"
    printf '#!/bin/bash\necho "a  b"\n' | wr "$T/s06.sh"
    printf '#!/bin/bash\nhead -3 "${BASH_SOURCE[0]}"\n# c2\n' | wr "$T/s07.sh"
    printf '#!/usr/bin/env bash\n# new\necho t\n' | wr "$T/bin/tool"
    printf '#!/bin/bash\n# usage: s08.sh [-h]\nSELF="$0"\nsed -n 2p "$SELF"\n' | wr "$T/s08.sh"
    printf '#!/bin/bash\n# c2\nSELF="${BASH_SOURCE[0]}"\nwhile read -r l; do echo "$l"; done < "$SELF"\n' | wr "$T/s09.sh"
    printf '#!/bin/bash\n# c2\necho "usage: $0"\ngrep -c x f\n' | wr "$T/s10.sh"
    printf '#!/bin/bash\n# c2\nexec 3<"$0"\nwhile read -r -u3 l; do echo "$l"; done\n' | wr "$T/s11.sh"
    printf '#!/bin/bash\n# c2\ncommand sed -n 2p "$0"\n' | wr "$T/s12.sh"
    printf '#!/bin/bash\n# c2\nSELF="$0"\nexec 4<"$SELF"\nhead -n 2 <&4\n' | wr "$T/s13.sh"
    printf '#!/bin/bash\n# c2\nSELF="${BASH_SOURCE[0]}"\ntimeout 5 env LC_ALL=C sed -n 2p "$SELF"\n' | wr "$T/s14.sh"
    cm "$T" head
    run_check "$T" --base "$BASE_T" --head HEAD
    expect_file "shell: a comment edit is non_executable" s01.sh non_executable comments_and_whitespace_only
    expect_file "shell: a shebang change is executable" s02.sh executable directive_comment
    expect_file "shell: a shellcheck directive change is executable" s03.sh executable directive_comment
    expect_file "shell: a script that prints itself makes its comments executable" s04.sh executable source_reader:shell_self_read
    expect_file "shell: awk's \$0 inside single quotes is not a self-read" s05.sh non_executable comments_and_whitespace_only
    expect_file "shell: whitespace inside a quoted word is executable" s06.sh executable token_change
    expect_file "shell: a BASH_SOURCE self-read makes its comments executable" s07.sh executable source_reader:shell_self_read
    expect_file "shell: an extensionless script is read by its shebang" bin/tool non_executable comments_and_whitespace_only
    # The path travels through a variable (an in-repo installer script
    # has this shape), so the read is refused without tracing the data flow.
    expect_file "shell: SELF=\"\$0\"; sed -n 2p \"\$SELF\" is a possible self-read" s08.sh executable source_reader:possible_self_read
    expect_file "shell: a < redirect from a variable in a script naming BASH_SOURCE is a possible self-read" s09.sh executable source_reader:possible_self_read
    expect_file "shell: \$0 in a usage line with readers taking only literals stays non_executable" s10.sh non_executable comments_and_whitespace_only
    # tree-sitter-bash puts the descriptor of a numbered redirect BEFORE its
    # operator, and a wrapper (command, env, timeout, ...) runs the reader.
    expect_file "shell: a numbered redirect exec 3<\"\$0\" read by read -u3 is a self-read" s11.sh executable source_reader:shell_self_read
    expect_file "shell: a reader under the command wrapper is a self-read" s12.sh executable source_reader:shell_self_read
    expect_file "shell: a numbered exec 4< redirect from a variable is a possible self-read" s13.sh executable source_reader:possible_self_read
    expect_file "shell: a reader among a wrapper's arguments (timeout ... env ... sed) is a possible self-read" s14.sh executable source_reader:possible_self_read

    # Round-8 sh9 / sh10: tree-sitter-bash ends a heredoc on a line bash does
    # not (`EOF ` with a trailing space; `<<-` with a SPACE-indented `EOF`),
    # so a "comment" it parses after that line is heredoc DATA to bash.
    H="$SANDBOX/heredoc"; mkrepo "$H"
    printf '#!/bin/bash\ncat <''<EOF\nline\nEOF \n# secret one\nEOF\n' | wr "$H/h1.sh"
    printf '#!/bin/bash\ncat <''<-EOF\nline\n  EOF\n# secret one\n\tEOF\n' | wr "$H/h2.sh"
    printf '#!/bin/bash\ncat <''<EOF\nline\nEOF\n# one\necho done\n' | wr "$H/h3.sh"
    printf "#!/bin/bash\ncat <""<-'E F' >/dev/null\n\tline\n\tE F\n# one\n" | wr "$H/h4.sh"
    printf "#!/bin/bash\ncat <""<'E\\\\OF'\nx\nEOF\n# one\nE\\\\OF\n" | wr "$H/h5.sh"
    printf '#!/bin/bash\ncat <''<"E\\OF"\nx\nEOF\n# one\nE\\OF\n' | wr "$H/h6.sh"
    printf '#!/bin/bash\ncat <''<\\EOF\nx\nEOF\n# one\n' | wr "$H/h7.sh"
    cm "$H" base; BASE_H="$(git -C "$H" rev-parse HEAD)"
    printf '#!/bin/bash\ncat <''<EOF\nline\nEOF \n# secret two\nEOF\n' | wr "$H/h1.sh"
    printf '#!/bin/bash\ncat <''<-EOF\nline\n  EOF\n# secret two\n\tEOF\n' | wr "$H/h2.sh"
    printf '#!/bin/bash\ncat <''<EOF\nline\nEOF\n# two\necho done\n' | wr "$H/h3.sh"
    printf "#!/bin/bash\ncat <""<-'E F' >/dev/null\n\tline\n\tE F\n# two\n" | wr "$H/h4.sh"
    printf "#!/bin/bash\ncat <""<'E\\\\OF'\nx\nEOF\n# two\nE\\\\OF\n" | wr "$H/h5.sh"
    printf '#!/bin/bash\ncat <''<"E\\OF"\nx\nEOF\n# two\nE\\OF\n' | wr "$H/h6.sh"
    printf '#!/bin/bash\ncat <''<\\EOF\nx\nEOF\n# two\n' | wr "$H/h7.sh"
    cm "$H" head
    run_check "$H" --base "$BASE_H" --head HEAD
    expect_file "shell: r8 sh9: a heredoc 'EOF ' line (trailing space) does not end it" h1.sh executable shell_heredoc_terminator_ambiguous
    expect_file "shell: r8 sh10: a dash-form heredoc's space-indented EOF does not end it" h2.sh executable shell_heredoc_terminator_ambiguous
    expect_file "shell: a comment after a well-formed heredoc stays non_executable" h3.sh non_executable comments_and_whitespace_only
    expect_file "shell: a comment after a tab-indented quoted dash-form heredoc stays non_executable" h4.sh non_executable comments_and_whitespace_only
    # Round-9 F4: a backslash INSIDE quotes is kept by bash's quote removal,
    # so the delimiter is `E\OF` and the `EOF` line is body, not the end.
    expect_file "shell: r9 F4: a single-quoted delimiter keeps its backslash" h5.sh executable shell_heredoc_terminator_ambiguous
    expect_file "shell: r9 F4: a double-quoted delimiter keeps a backslash before a plain char" h6.sh executable shell_heredoc_terminator_ambiguous
    expect_file "shell: an unquoted backslash delimiter (the \\\\EOF idiom) is quote-removed" h7.sh non_executable comments_and_whitespace_only

    # The string literal spans a line break, so its text carries the line
    # ending: read off disk the CRLF checkout differs from the base, while the
    # bytes git would store (autocrlf cleans CRLF to LF) differ only in the
    # comment.
    X="$SANDBOX/crlf"; mkrepo "$X"
    printf '#!/bin/bash\n# old\necho "a\nb"\n' | wr "$X/s.sh"; cm "$X" base
    git -C "$X" config core.autocrlf true
    printf '#!/bin/bash\r\n# new\r\necho "a\r\nb"\r\n' | wr "$X/s.sh"
    run_check "$X" --base HEAD
    expect_file "shell: a CRLF checkout is compared as git would store it" s.sh non_executable comments_and_whitespace_only
  else
    ts_unmeasured shell-core
  fi
fi

if mc_sec shell-r33; then
  echo "== shell-r33"
  if [ "$TS_OK" = 1 ]; then
    # Round 33 F1/F4: bash splits words on space, tab and newline only, and a
    # `#` starts a comment only at the start of a word, outside arithmetic and
    # backquotes. tree-sitter-bash is wider on each.
    W="$SANDBOX/r33sh"; mkrepo "$W"
    printf 'set -- a\vb; echo $#\n' | wr "$W/w1.sh"
    printf 'set -- a\rb; echo $#\n' | wr "$W/w2.sh"
    printf 'echo a\r#AAA\n' | wr "$W/w3.sh"
    printf 'echo a\\\n#AAA\n' | wr "$W/w4.sh"
    printf 'echo $((1 #AAA\n))\n' | wr "$W/w5.sh"
    printf 'echo `a #AAA`\n' | wr "$W/w6.sh"
    printf '# AAA\necho hi \\\n  there\n' | wr "$W/w7.sh"
    printf 'echo a ;#AAA\n' | wr "$W/w8.sh"
    # Round 33 F2: every way bash names its own script or text.
    printf '# AAA\ngrep -o x "${@:0:1}"\n' | wr "$W/n1.sh"
    printf '# AAA\ngrep x "$BASH_ARGV0"\n' | wr "$W/n2.sh"
    printf '# AAA\nf() { caller 0; }; set -- $(f); grep x "$3"\n' | wr "$W/n3.sh"
    printf "# AAA\nIFS= read -rd '' s < /proc/self/fd/255; echo \"\${s:2:3}\"\n" | wr "$W/n4.sh"
    printf '# AAA\ncat "$_"\n' | wr "$W/n5.sh"
    printf '# AAA\nx=0; cat "${!x}"\n' | wr "$W/n6.sh"
    printf "# AAA\neval 'sed -n 1p \$0'\n" | wr "$W/n7.sh"
    printf "# AAA\ntrap 'rm -f \"\$t\"' EXIT\necho hi\n" | wr "$W/n8.sh"
    printf '# AAA\neval "$cmd"\n' | wr "$W/n9.sh"
    printf '# AAA\nc=cat; $c "$0"\n' | wr "$W/n10.sh"
    printf '# AAA\nsed -n 1p n11.sh\n' | wr "$W/n11.sh"
    printf "# AAA\nbash -c 'sed -n 1p \"\$1\"' _ \"\$0\"\n" | wr "$W/n12.sh"
    printf '# AAA\nSELF="$0"; cp "$SELF" t; cat t\n' | wr "$W/n13.sh"
    printf '# AAA\nenv "$tool" "$0"\n' | wr "$W/n14.sh"
    printf "# AAA\nalias v='sed -n 1p \$0'\n" | wr "$W/n15.sh"
    printf "# AAA\neval 'if'\n" | wr "$W/n16.sh"
    printf "# AAA\ncommand eval 'sed -n 1p \$0'\n" | wr "$W/n17.sh"
    cm "$W" base; BASE_W="$(git -C "$W" rev-parse HEAD)"
    printf 'set -- a b; echo $#\n' | wr "$W/w1.sh"
    printf 'set -- a b; echo $#\n' | wr "$W/w2.sh"
    for f in w3 w4 w5 w6 w7 w8 n1 n2 n3 n4 n5 n6 n7 n8 n9 n10 n11 n12 n13 n14 n15 n16 n17; do
      sed 's/AAA/BBB/' "$W/$f.sh" > "$W/$f.tmp" && mv "$W/$f.tmp" "$W/$f.sh"
    done
    cm "$W" head
    run_check "$W" --base "$BASE_W" --head HEAD
    expect_file "shell: r33 F1: a VT bash keeps in the word is not blank" w1.sh executable token_change
    expect_file "shell: r33 F1: a CR bash keeps in the word is not blank" w2.sh executable token_change
    expect_file "shell: r33 F1: a # after a CR is part of the word" w3.sh executable token_change
    expect_file "shell: r33 F1: a # after a backslash-newline continuation is part of the word" w4.sh executable
    expect_file "shell: r33 F4: a # inside \$(( )) reaches the arithmetic evaluator" w5.sh executable token_change
    expect_file "shell: r33 F4: a comment that swallows a closing backquote is a parse error, never proven" w6.sh unknown parse_error_both
    expect_file "shell: r33: a line continuation between tokens stays blank" w7.sh non_executable comments_and_whitespace_only
    expect_file "shell: r33: a # after a metacharacter is a comment" w8.sh non_executable comments_and_whitespace_only
    expect_file "shell: r33 F2: \${@:0:1} names the script" n1.sh executable source_reader:shell_self_read
    expect_file "shell: r33 F2: BASH_ARGV0 names the script" n2.sh executable source_reader:shell_self_read
    expect_file "shell: r33 F2: caller names the script" n3.sh executable source_reader:possible_self_read
    expect_file "shell: r33 F2: /proc/self/fd/255 is the script" n4.sh executable source_reader:shell_self_read
    expect_file "shell: r33 F2: \$_ names the script at startup" n5.sh executable source_reader:shell_self_read
    expect_file "shell: r33 F2: an indirect \${!x} may name the script" n6.sh executable source_reader:shell_self_read
    expect_file "shell: r33 F2: eval'd literal text is checked like the script" n7.sh executable source_reader:shell_self_read
    expect_file "shell: r33 F2: a trap's literal text with no self-read stays non_executable" n8.sh non_executable comments_and_whitespace_only
    expect_file "shell: r33 F2: eval of computed text refuses the proof" n9.sh executable source_reader:shell_evaluated_text_not_literal
    expect_file "shell: r33 F2: a computed command name counts as a reader" n10.sh executable source_reader:shell_self_read
    expect_file "shell: r33 F2: a literal naming the file names the script" n11.sh executable source_reader:shell_self_read
    expect_file "shell: r33 F2: a shell's -c text is checked like the script" n12.sh executable source_reader:
    expect_file "shell: r33 F2: cp is a reader (a copy carries the text)" n13.sh executable source_reader:possible_self_read
    expect_file "shell: r33 F2: a wrapper handed a computed word counts as a reader" n14.sh executable source_reader:shell_self_read
    expect_file "shell: r33 F2: an alias's text after = is checked like the script" n15.sh executable source_reader:shell_self_read
    expect_file "shell: r33 F2: evaluated text that does not parse refuses the proof" n16.sh executable source_reader:shell_evaluated_text_unparsed
    expect_file "shell: r33 F2: an evaluator under a wrapper is checked too" n17.sh executable source_reader:shell_self_read
    # Round 34 F1: a `#` reaches the arithmetic evaluator, or stays in the
    # word, in every arithmetic and `${ }` context -- but a command
    # substitution or a loop body nested there starts a fresh command line.
    # F2: an option word before evaluated text hides nothing.
    V="$SANDBOX/r34sh"; mkrepo "$V"
    printf 'for ((i=0; i<1; i++ #AAA\n)); do :; done\n' | wr "$V/a1.sh"
    printf 'x=abc; echo ${x:1 #AAA\n}\n' | wr "$V/a2.sh"
    printf 'a=(1 2); echo ${a[1 #AAA\n]}\n' | wr "$V/a3.sh"
    printf 'a[1 #AAA\n]=2\n' | wr "$V/a4.sh"
    printf 'for ((i=0; i<1; i++)); do\n  # AAA\n  :\ndone\n' | wr "$V/a5.sh"
    printf 'echo ${x:-$(echo hi #AAA\n)}\n' | wr "$V/a6.sh"
    printf "# AAA\ntrap -- 'sed -n 1p \$0' EXIT\n" | wr "$V/e1.sh"
    printf "# AAA\neval -- 'sed -n 1p \$0'\n" | wr "$V/e2.sh"
    printf "# AAA\nbash -c -o errexit 'sed -n 1p \$0'\n" | wr "$V/e3.sh"
    printf "# AAA\ntrap -- 'echo hi' EXIT\n" | wr "$V/e4.sh"
    cm "$V" base; BASE_V="$(git -C "$V" rev-parse HEAD)"
    for f in a1 a2 a3 a4 a5 a6 e1 e2 e3 e4; do
      sed 's/AAA/BBB/' "$V/$f.sh" > "$V/$f.tmp" && mv "$V/$f.tmp" "$V/$f.sh"
    done
    cm "$V" head
    run_check "$V" --base "$BASE_V" --head HEAD
    expect_file "shell: r34 F1: a # in a C-style for header reaches the arithmetic evaluator" a1.sh executable token_change
    expect_file "shell: r34 F1: a # in a \${x:off} offset is part of the expansion" a2.sh executable token_change
    expect_file "shell: r34 F1: a # in an expansion's subscript is part of it" a3.sh executable token_change
    expect_file "shell: r34 F1: a # in an assignment's subscript is part of it" a4.sh executable token_change
    expect_file "shell: r34: a comment in a C-style for body stays non_executable" a5.sh non_executable comments_and_whitespace_only
    expect_file "shell: r34: a comment in a command substitution inside \${ } stays non_executable" a6.sh non_executable comments_and_whitespace_only
    expect_file "shell: r34 F2: trap -- text is checked like the script" e1.sh executable source_reader:shell_self_read
    expect_file "shell: r34 F2: eval -- text is checked like the script" e2.sh executable source_reader:shell_self_read
    expect_file "shell: r34 F2: an option taking an argument before -c text hides nothing" e3.sh executable source_reader:shell_self_read
    expect_file "shell: r34: trap -- text with no self-read stays non_executable" e4.sh non_executable comments_and_whitespace_only
  else
    ts_unmeasured shell-r33
  fi
fi

if mc_sec shell-r35; then
  echo "== shell-r35"
  if [ "$TS_OK" = 1 ]; then
    # Round 35 F1/F5: a compound assignment's [ ] key keeps a # in the key,
    # and only a C-style for's header -- not what follows its )) -- is
    # arithmetic. F2: verbose mode echoes every line read, comments included.
    U="$SANDBOX/r35sh"; mkrepo "$U"
    printf 'declare -A m=([k #AAA\n]=v); echo "${!m[@]}"\n' | wr "$U/b1.sh"
    printf 'a=([0 #AAA\n]=x); echo "${a[0]}"\n' | wr "$U/b2.sh"
    printf 'a=(1 #AAA\n2); echo ${#a[@]}\n' | wr "$U/b3.sh"
    printf 'for ((i=0; i<1; i++)) # AAA\ndo :; done\n' | wr "$U/b4.sh"
    printf 'for ((i=0; i<1; i++))\n# AAA\ndo :; done\n' | wr "$U/b5.sh"
    printf 'set -v\n# AAA\necho hi\n' | wr "$U/v1.sh"
    printf '#!/bin/bash -v\n# AAA\necho hi\n' | wr "$U/v2.sh"
    printf 'set -o verbose\n# AAA\necho hi\n' | wr "$U/v3.sh"
    printf 'command set -ev\n# AAA\necho hi\n' | wr "$U/v4.sh"
    printf 'shopt -os verbose\n# AAA\necho hi\n' | wr "$U/v5.sh"
    printf "eval 'set -v'\n# AAA\necho hi\n" | wr "$U/v6.sh"
    printf 'set "$opts"\n# AAA\necho hi\n' | wr "$U/v7.sh"
    printf 'set -euo pipefail\nset -- "$@"\n# AAA\necho hi\n' | wr "$U/v8.sh"
    cm "$U" base; BASE_U="$(git -C "$U" rev-parse HEAD)"
    for f in b1 b2 b3 b4 b5 v1 v2 v3 v4 v5 v6 v7 v8; do
      sed 's/AAA/BBB/' "$U/$f.sh" > "$U/$f.tmp" && mv "$U/$f.tmp" "$U/$f.sh"
    done
    cm "$U" head
    run_check "$U" --base "$BASE_U" --head HEAD
    expect_file "shell: r35 F1: a # in an associative array's [ ] key is part of the key" b1.sh executable token_change
    expect_file "shell: r35 F1: a # in an indexed array's [ ] key reaches the arithmetic evaluator" b2.sh executable token_change
    expect_file "shell: r35: a comment between array elements stays non_executable" b3.sh non_executable comments_and_whitespace_only
    expect_file "shell: r35 F5: a comment after a C-style for header is a comment" b4.sh non_executable comments_and_whitespace_only
    expect_file "shell: r35 F5: a comment line between a C-style for header and do is a comment" b5.sh non_executable comments_and_whitespace_only
    expect_file "shell: r35 F2: set -v echoes every comment" v1.sh executable source_reader:shell_verbose
    expect_file "shell: r35 F2: a -v shebang flag echoes every comment" v2.sh executable source_reader:shell_verbose
    expect_file "shell: r35 F2: set -o verbose echoes every comment" v3.sh executable source_reader:shell_verbose
    expect_file "shell: r35 F2: a wrapped set with v in an option cluster echoes every comment" v4.sh executable source_reader:shell_verbose
    expect_file "shell: r35 F2: shopt -o verbose echoes every comment" v5.sh executable source_reader:shell_verbose
    expect_file "shell: r35 F2: set -v in evaluated text echoes every comment" v6.sh executable source_reader:shell_verbose
    expect_file "shell: r35 F2: a set option word that is not a literal may turn verbose on" v7.sh executable source_reader:shell_verbose
    expect_file "shell: r35: set -euo pipefail and set -- stay non_executable" v8.sh non_executable comments_and_whitespace_only
    # Round 36: every argument word is read whatever its node type, $'..' is
    # decoded as bash decodes it, a #! long option is a flag, and only an
    # unquoted, unescaped ] closes an array key.
    X="$SANDBOX/r36sh"; mkrepo "$X"
    printf '#!/bin/bash --verbose\n# AAA\necho hi\n' | wr "$X/c1.sh"
    printf "set \$'-v'\n# AAA\necho hi\n" | wr "$X/c2.sh"
    printf 'declare -A m=([k"]" #AAA\n]=v); echo "${!m[@]}"\n' | wr "$X/c3.sh"
    printf 'declare -A m=([k\\] #AAA\n]=v); echo "${!m[@]}"\n' | wr "$X/c4.sh"
    printf 'declare -A m=([k]=v) #AAA\necho "${!m[@]}"\n' | wr "$X/c5.sh"
    printf "# AAA\neval \$'sed -n 1p \$0'\n" | wr "$X/c6.sh"
    printf "# AAA\n\$'cat' \"\$0\"\n" | wr "$X/c7.sh"
    printf "set -- \$'x\\ty'\n# AAA\necho hi\n" | wr "$X/c8.sh"
    printf '# AAA\nx=1; eval "echo $((x))"\n' | wr "$X/c9.sh"
    cm "$X" base; BASE_X="$(git -C "$X" rev-parse HEAD)"
    for f in c1 c2 c3 c4 c5 c6 c7 c8 c9; do
      sed 's/AAA/BBB/' "$X/$f.sh" > "$X/$f.tmp" && mv "$X/$f.tmp" "$X/$f.sh"
    done
    cm "$X" head
    run_check "$X" --base "$BASE_X" --head HEAD
    expect_file "shell: r36: a #! --verbose long option echoes every comment" c1.sh executable source_reader:shell_verbose
    expect_file "shell: r36: an ANSI-C quoted set option is read decoded" c2.sh executable source_reader:shell_verbose
    expect_file "shell: r36: a quoted ] does not close an array key" c3.sh executable token_change
    expect_file "shell: r36: an escaped ] does not close an array key" c4.sh executable token_change
    expect_file "shell: r36: a comment after a closed array is a comment" c5.sh non_executable comments_and_whitespace_only
    expect_file "shell: r36: ANSI-C quoted eval text is checked like the script" c6.sh executable source_reader:shell_self_read
    expect_file "shell: r36: an ANSI-C quoted command name is read decoded" c7.sh executable source_reader:shell_self_read
    expect_file "shell: r36: set -- with an ANSI-C word stays non_executable" c8.sh non_executable comments_and_whitespace_only
    expect_file "shell: r36: evaluated text holding an arithmetic expansion is not a literal" c9.sh executable source_reader:shell_evaluated_text_not_literal
    # Round 37: a bash word is a C string (cut at its first NUL), and a word
    # bash expands as a pattern is not a literal.
    Y="$SANDBOX/r37sh"; mkrepo "$Y"
    printf "# AAA\n\$'cat\\\\0x' \"\$0\"\n" | wr "$Y/d1.sh"
    printf "# AAA\n\$'cat\\\\x00' \"\$0\"\n" | wr "$Y/d2.sh"
    printf '# AAA\n/bin/ca? "$0"\n' | wr "$Y/d3.sh"
    printf '# AAA\n/bin/c[a]t "$0"\n' | wr "$Y/d4.sh"
    printf '# AAA\n[ -f "$0" ] && echo "$0" \\*\n' | wr "$Y/d5.sh"
    printf '# AAA\neval echo {a,b}\n' | wr "$Y/d6.sh"
    cm "$Y" base; BASE_Y="$(git -C "$Y" rev-parse HEAD)"
    for f in d1 d2 d3 d4 d5 d6; do
      sed 's/AAA/BBB/' "$Y/$f.sh" > "$Y/$f.tmp" && mv "$Y/$f.tmp" "$Y/$f.sh"
    done
    cm "$Y" head
    run_check "$Y" --base "$BASE_Y" --head HEAD
    expect_file "shell: r37: a NUL octal escape cuts an ANSI-C command name" d1.sh executable source_reader:shell_self_read
    expect_file "shell: r37: a NUL hex escape cuts an ANSI-C command name" d2.sh executable source_reader:shell_self_read
    expect_file "shell: r37: a ? pattern command name is not a literal" d3.sh executable source_reader:shell_self_read
    expect_file "shell: r37: a [ ] pattern command name is not a literal" d4.sh executable source_reader:shell_self_read
    expect_file "shell: r37: a test and an escaped * stay non_executable" d5.sh non_executable comments_and_whitespace_only
    expect_file "shell: r37: evaluated text with a brace expansion is not a literal" d6.sh executable source_reader:shell_evaluated_text_not_literal
    # Round 38: tree-sitter's brace_expression pieces are unquoted bytes too.
    Y="$SANDBOX/r38sh"; mkrepo "$Y"
    printf '# AAA\npython{3..3} -c "print(open(\\"$0\\").read())"\n' | wr "$Y/e1.sh"
    printf '# AAA\neval echo {1..2}\n' | wr "$Y/e2.sh"
    cm "$Y" base; BASE_Y="$(git -C "$Y" rev-parse HEAD)"
    for f in e1 e2; do
      sed 's/AAA/BBB/' "$Y/$f.sh" > "$Y/$f.tmp" && mv "$Y/$f.tmp" "$Y/$f.sh"
    done
    cm "$Y" head
    run_check "$Y" --base "$BASE_Y" --head HEAD
    expect_file "shell: r38: a numeric brace range in a command name is a pattern" e1.sh executable source_reader:shell_self_read
    expect_file "shell: r38: evaluated text with a numeric brace range is not a literal" e2.sh executable source_reader:shell_evaluated_text_not_literal
  else
    ts_unmeasured shell-r35
  fi
fi

# ---- section: vendored ----------------------------------------------------------
if mc_sec vendored; then
  echo "== vendored"
  CANON="$HERE/../../../scripts/lib"
  for f in coord-tenant-credential.sh bash_resolve.py git_scope.py; do
    if [ ! -r "$CANON/$f" ]; then
      unmeasured "vendored: $f (no canonical scripts/lib/$f beside this checkout)"
    elif cmp -s "$HERE/lib/$f" "$CANON/$f"; then
      ok "vendored: lib/$f is byte-identical to scripts/lib/$f"
    else
      bad "vendored: lib/$f is byte-identical to scripts/lib/$f" "re-copy it: cp scripts/lib/$f .claude/skills/followup-sweep/lib/$f"
    fi
  done
  if bash -n "$HERE/lib/stage-coord-bearer.sh" 2>/dev/null; then ok "vendored: lib/stage-coord-bearer.sh parses"; else bad "vendored: lib/stage-coord-bearer.sh parses"; fi
fi

# ---- section: census ------------------------------------------------------------
if mc_sec census; then
  echo "== census"
  TOP="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)" || TOP=""
  if [ -z "$TOP" ]; then
    unmeasured "census: $HERE is not inside a git work tree"
  else
    wr "$SANDBOX/census.py" <<'PYEOF'
"""SOURCE_READERS census: of the repo's tracked Python files, how many name an
identifier in coord's reader list (copied from
crates/coord/src/pr_merge/migration_classifier.rs SOURCE_READERS at
qontinui-coord 9e5bec3dc), under `migration` (the whole list) and `general`
(the list without __name__). A file naming one is refused outright by coord's
proof under that profile."""
import io, subprocess, sys, tokenize

READERS = [
    "__doc__", "__file__", "__loader__", "__spec__", "__cached__", "__name__",
    "__module__", "__code__", "__dict__", "__globals__", "__builtins__",
    "__getattribute__", "__import__", "globals", "locals", "vars", "eval",
    "exec", "compile", "getattr", "setattr", "open", "io", "os", "sys",
    "pathlib", "Path", "inspect", "getdoc", "getsource", "getsourcelines",
    "get_source", "linecache", "tokenize", "ast", "importlib", "modules",
    "_getframe", "f_globals", "f_locals", "f_back", "attrgetter",
    "methodcaller", "read_text", "read_bytes", "format", "format_map", "script",
    "get_revision", "longdoc", "doc", "traceback", "extract_stack", "stack",
    "currentframe", "fileinput", "pkgutil", "get_data", "subprocess", "runpy",
    "codecs", "doctest", "run_docstring_examples", "testmod", "DocTestFinder",
    "pydoc", "render_doc", "help", "dis", "getmembers", "Operations",
    "MigrationContext", "co_consts", "f_code", "gi_code", "cr_code", "ag_code",
    "tb_frame", "__traceback__", "gc", "get_referents", "get_objects", "pickle",
    "marshal", "ctypes", "lineno", "f_lineno", "tb_lineno", "co_firstlineno",
    "co_lines", "co_positions", "catch_warnings", "showwarning", "findCaller",
    "vformat", "Formatter", "get_field", "metaclass", "__prepare__",
    "__init_subclass__",
]
MIGRATION = set(READERS)
GENERAL = MIGRATION - {"__name__"}

top = sys.argv[1]
names = subprocess.run(["git", "-C", top, "ls-files", "-z", "--", "*.py"], stdout=subprocess.PIPE, check=True).stdout
files = [n.decode("utf-8", "surrogateescape") for n in names.split(b"\0") if n]
refused_m = refused_g = unreadable = 0
for rel in files:
    try:
        with open(top + "/" + rel, "rb") as fh:
            toks = list(tokenize.tokenize(io.BytesIO(fh.read()).readline))
    except (OSError, SyntaxError, tokenize.TokenError, UnicodeDecodeError):
        unreadable += 1
        continue
    found = {t.string for t in toks if t.type == tokenize.NAME}
    refused_m += bool(found & MIGRATION)
    refused_g += bool(found & GENERAL)
print("SOURCE_READERS census: repo=%s readers=%d py_files=%d refused_migration=%d refused_general=%d "
      "only_through___name__=%d untokenizable=%d" % (top.rstrip("/").split("/")[-1], len(READERS), len(files),
                                                     refused_m, refused_g, refused_m - refused_g, unreadable))
PYEOF
    CENSUS="$("$PY" "$(native_path_m "$SANDBOX/census.py")" "$(native_path_m "$TOP")" 2>&1 | tr -d '\r')"
    printf '%s\n' "$CENSUS"
    case "$CENSUS" in
      "SOURCE_READERS census: "*" py_files=0 "*) bad "census: the census read a non-empty Python population" "$CENSUS" ;;
      "SOURCE_READERS census: "*) ok "census: the census read a non-empty Python population" ;;
      *) bad "census: the census read a non-empty Python population" "$CENSUS" ;;
    esac
  fi
fi

# ---- mutation discharge (the full, graded run only) ------------------------------
if mc_graded; then
  echo "== mutation discharge"
  # A gate's skipped mutants are named from the gate's own code, never from a
  # hand list beside it: such a list went stale once (M86 and M88 sat behind a
  # gate whose UNMEASURED line never named them).
  # A shut gate names its mutants from its text, so the text must name exactly
  # the mutants the gate declares when it runs. Two checks, on every graded run:
  # - statically, gate open or shut: the gate must exist and hold a call, and
  #   the number of mc_expect_red mentions a regex finds in its declare -f
  #   text must equal the number of ids read from `mc_expect_red "M<n> `. This
  #   reads text, not bash: an unusual spelling of the call (quoted, escaped,
  #   two names sharing one delimiter) can be miscounted either way, which is
  #   why it is not the authority;
  # - when the gate runs: the sorted list of labels it registered with the
  #   library (MC_LABEL_KEYS) must equal the sorted list of ids its text names
  #   (so a duplicate counts), and the number it declared (MC_DECLARED) must
  #   equal the number named. This is the authority: it sees every mutant the
  #   gate declares in its own shell however the call is spelled, and an id
  #   its text names that nothing registered when it ran. A mutant declared in
  #   a subshell leaves no trace in either. The pack's CI run opens every
  #   tree-sitter gate.
  gated_mutants() { # <1|0> <function> <reporter> <why>
    local _gm_body _gm_ids _gm_calls _gm_named _gm_before _gm_keys _gm_ran _gm_want _gm_rc _gm_k
    if ! declare -F "$2" >/dev/null; then
      bad "gate $2 is not a defined function; its mutants can be neither run nor named"
      return
    fi
    _gm_body="$(declare -f "$2")"
    _gm_calls="$(printf '%s\n' "$_gm_body" | grep -oE '(^|[^[:alnum:]_])mc_expect_red([^[:alnum:]_]|$)' | wc -l | tr -d ' ')"
    # The label must follow the call, so an "M<n> inside a sed expression is no id.
    _gm_ids="$(printf '%s\n' "$_gm_body" | grep -oE '(^|[^[:alnum:]_])mc_expect_red "M[0-9]+ ' | sed 's/^.*mc_expect_red "//' | tr -d '\n')"
    _gm_ids="${_gm_ids% }"
    _gm_named="$(printf '%s' "$_gm_ids" | wc -w | tr -d ' ')"
    if [ "$_gm_calls" = 0 ]; then
      bad "gate $2 holds no mc_expect_red call; an empty gate proves nothing"
    elif [ "$_gm_calls" != "$_gm_named" ]; then
      bad "gate $2 holds $_gm_calls mc_expect_red call(s) but only $_gm_named carry a double-quoted \"M<n> \" label; a shut gate could not name the rest"
    fi
    if [ "$1" = 1 ]; then
      _gm_before="$MC_DECLARED"
      _gm_keys="${#MC_LABEL_KEYS[@]}"
      "$2"; _gm_rc=$?
      # Each key is written %q-quoted, one per line, so a key holding a newline
      # or a space stays ONE token and can never pass for two ids. Sorted, so
      # call order does not matter; duplicates still count.
      _gm_ran="$(for _gm_k in "${MC_LABEL_KEYS[@]:$_gm_keys}"; do printf '%q\n' "$_gm_k"; done | sort | tr '\n' ' ')"
      _gm_want="$(for _gm_k in $_gm_ids; do printf '%q\n' "$_gm_k"; done | sort | tr '\n' ' ')"
      if [ "$((MC_DECLARED - _gm_before))" != "$_gm_named" ] || [ "${_gm_ran% }" != "${_gm_want% }" ]; then
        bad "gate $2 declared $((MC_DECLARED - _gm_before)) mutant(s) registering [${_gm_ran% }] when it ran, but its text names $_gm_named: [${_gm_want% }]; shut, its line would not match what ran"
      fi
      return "$_gm_rc"
    fi
    "$3" "mutations ${_gm_ids:-<none>} ($4)"
  }
  # Without the pack, a mutant whose --only names ONLY sections in TS_SECTIONS
  # re-runs no case and exits 1 -- a red that proves nothing. Such a mutant
  # belongs behind a gate; this refuses one that is not, instead of counting
  # its vacuous red. With the pack it is the library's mc_expect_red, unchanged.
  eval "_mc_expect_red_lib$(declare -f mc_expect_red | sed '1s/^mc_expect_red//')"
  mc_expect_red() {
    if [ "$TS_OK" != 1 ]; then
      local _a _next=0 _only="" _sec _ts_only=1
      for _a in "$@"; do
        [ "$_next" = 1 ] && { _only="$_a"; _next=0; }
        [ "$_a" = --only ] && _next=1
      done
      if [ -n "$_only" ]; then
        for _sec in ${_only//,/ }; do
          case " $TS_SECTIONS " in *" $_sec "*) ;; *) _ts_only=0 ;; esac
        done
        if [ "$_ts_only" = 1 ]; then
          MC_DECLARED=$((MC_DECLARED + 1))
          mc_bad "$1 -- every section its --only names ($_only) needs the tree-sitter pack, which is absent here, so its re-run asserts nothing and its red would prove nothing; gate it on TS_OK"
          return
        fi
      fi
    fi
    _mc_expect_red_lib "$@"
  }
  M0="$SANDBOX/m0"; mkdir -p "$M0"; cp "$SUBJECT_REAL" "$M0/"
  if [ "$TS_OK" = 1 ]; then
    mc_only_control "$M0/exec-equivalence.py" -- bash "$SELF"
  else
    # Without the pack every case in TS_SECTIONS is UNMEASURED, so each of
    # those sections alone runs no case, and the control would read "asserted
    # nothing" as a broken section and turn a run the header calls UNMEASURED
    # red. It proves every other section and reports these as unmeasured,
    # which is a failure where the pack is required (ts_unmeasured).
    MC_ONLY_SECTIONS_ALL="$MC_ONLY_SECTIONS"
    MC_ONLY_SECTIONS=""
    for _s in $MC_ONLY_SECTIONS_ALL; do
      case " $TS_SECTIONS " in *" $_s "*) ;; *) MC_ONLY_SECTIONS="$MC_ONLY_SECTIONS $_s" ;; esac
    done
    mc_only_control "$M0/exec-equivalence.py" -- bash "$SELF"
    MC_ONLY_SECTIONS="$MC_ONLY_SECTIONS_ALL"
    ts_unmeasured "the section isolation control for $TS_SECTIONS"
  fi
  mc_expect_red "M1 the >>> refusal never fires" "$SUBJECT_REAL" \
    's/            if b != h and (">>>" in b or ">>>" in h):/            if False:/' -- bash "$SELF" --only coord
  mc_expect_red "M2 coord's refusal reads non_executable" "$SUBJECT_REAL" \
    's/change.verdict, change.reason = EXECUTABLE, "coord_refused_proof:" + profile/change.verdict, change.reason = NON_EXECUTABLE, "coord_refused_proof:" + profile/' -- bash "$SELF" --only coord
  mc_expect_red "M3 an unreachable coord reads non_executable" "$SUBJECT_REAL" \
    's/change.verdict, change.reason = UNKNOWN, "coord_unreachable:" + err/change.verdict, change.reason = NON_EXECUTABLE, "coord_unreachable:" + err/' -- bash "$SELF" --only coord
  mc_expect_red "M4 undeclared code_paths read as outside" "$SUBJECT_REAL" \
    's/            return None, "code_paths_undeclared"/            return False, ""/' -- bash "$SELF" --only paths-core,paths-twin
  mc_expect_red "M5 an unknown-only verdict exits 0" "$SUBJECT_REAL" \
    's/overall, code = UNKNOWN, EXIT_UNKNOWN/overall, code = UNKNOWN, EXIT_OK/' -- bash "$SELF" --only paths-core
  mc_expect_red "M6 Markdown inside code_paths loses its no-inert-sublanguage verdict" "$SUBJECT_REAL" \
    's/^WHOLLY_SIGNIFICANT_EXTS = (".md", /WHOLLY_SIGNIFICANT_EXTS = (/' -- bash "$SELF" --only paths-core
  mc_expect_red "M7 a glob matches a path prefix instead of the whole path" "$SUBJECT_REAL" \
    's/if any(m.fullmatch(raw) for m in self.matchers):/if any(m.match(raw) for m in self.matchers):/' -- bash "$SELF" --only globs
  mc_expect_red "M8 a pack at another version is not refused" "$SUBJECT_REAL" \
    's/        if self.found != TREE_SITTER_PACK_PIN:/        if False:/' -- bash "$SELF" --only pack
  mc_expect_red "M17 a doc-included file whose code depends on block context is not refused" "$SUBJECT_REAL" \
    's/            if doc_layout_ambiguous(text_b) or doc_layout_ambiguous(text_h):/            if False:/' -- bash "$SELF" --only include-doc
  mc_expect_red "M18 a CommonMark HTML block is not read as block context" "$SUBJECT_REAL" \
    's/        if ln.lstrip(" ").startswith("<"):/        if False:/' -- bash "$SELF" --only include-doc
  # Reverting the opener rule leaves the belt (doc_layout_ambiguous) to catch
  # the change as block context, so the cases assert the code-block reason.
  mc_expect_red "M21 a backtick fence opener accepts a backtick in its info string" "$SUBJECT_REAL" \
    's/(\[^`\]\*)|(~{3,})/(.*)|(~{3,})/' -- bash "$SELF" --only include-doc,rust
  # Restoring Python's \s after a closer: NBSP / \x1c close the fence.
  mc_expect_red "M22 a fence closer may be followed by any Python whitespace" "$SUBJECT_REAL" \
    's/\[ \\t\]\*\$", ln)/\\s*$", ln)/' -- bash "$SELF" --only include-doc,rust
  mc_expect_red "M23 a footnote definition is not read as block context" "$SUBJECT_REAL" \
    's/        if ln.lstrip(" ").startswith("\[^"):/        if False:/' -- bash "$SELF" --only include-doc
  mc_expect_red "M24 a doc holding a lone CR is not refused" "$SUBJECT_REAL" \
    's/^    if LONE_CR_RE.search(text):$/    if False:/' -- bash "$SELF" --only include-doc
  mc_expect_red "M28 a doc include joined with another fragment of its item is not refused" "$SUBJECT_REAL" \
    's/^            if joined is not None:$/            if False:/' -- bash "$SELF" --only include-doc
  # Without U+200E in the set the lexer still sees an include, but cannot read
  # its argument, so the case's resolved reason reddens.
  mc_expect_red "M29 U+200E and U+200F are not Rust whitespace" "$SUBJECT_REAL" \
    '/^RUST_PATTERN_WHITE_SPACE = /s/\\u200e\\u200f//' -- bash "$SELF" --only include-doc
  mc_expect_red "M38 B: an opaque include does not make non-code files unknown" "$SUBJECT_REAL" \
    's/^        if opaque:$/        if False:/' -- bash "$SELF" --only include-opaque,include-r7,include-r9,include-r10,include-r11,include-link,include-r14,include-resolve,include-plain
  mc_expect_red "M39 B: another compile-time reader does not make non-code files unknown" "$SUBJECT_REAL" \
    's/^        elif readers:$/        elif False:/' -- bash "$SELF" --only include-readers
  mc_expect_red "M42 B: a .rs file's raw text (comments, string literals) is not scanned" "$SUBJECT_REAL" \
    's/^    rust_raw_text_scan(rs, text, {s for _, s in toks}, scan, "raw_text")$/    pass/' -- bash "$SELF" --only include-opaque,include-r7
  mc_expect_red "M43 B: the text of a plain-included file is not scanned" "$SUBJECT_REAL" \
    's/^                    rust_raw_text_scan(rs, target, excused, scan, label)$/                    pass/' -- bash "$SELF" --only include-r7,include-plain
  mc_expect_red "M44 B: git grep skips blobs it deems binary (-I)" "$SUBJECT_REAL" \
    's/"-l", "-z", "-a", "--no-textconv", "--no-color", "-F"/"-l", "-z", "-I", "--no-color", "-F"/' -- bash "$SELF" --only include-readers
  mc_expect_red "M49 B: git grep names are colored under color.grep=always" "$SUBJECT_REAL"     's/"--no-textconv", "--no-color", "-F"/"--no-textconv", "-F"/' -- bash "$SELF" --only include-readers
  mc_expect_red "M45 B: a name only a string escape spells is not refused" "$SUBJECT_REAL" \
    's/^    if decoded != text and len(_raw_names(decoded)) > len(names):$/    if False:/' -- bash "$SELF" --only include-opaque
  mc_expect_red "M46 B: a non-literal doc attribute value is not refused" "$SUBJECT_REAL" \
    's/^    if _RAW_DOC_NON_LITERAL_RE.search(text) or /    if False and /' -- bash "$SELF" --only include-opaque,include-r7
  mc_expect_red "M50 1a: a non-literal doc = in code tokens is not refused" "$SUBJECT_REAL" \
    's/^            scan.opaque.append((rs, "token:doc_attribute_non_literal"))$/            pass/' -- bash "$SELF" --only include-r7
  mc_expect_red "M51 1b: a macro_rules! attribute built from a metavariable is not refused" "$SUBJECT_REAL" \
    's/^            if w == "\$" and "attr" in regions:$/            if False:/' -- bash "$SELF" --only include-r7,include-r9,include-r10
  mc_expect_red "M52 2: only an include inside an attribute has its target raw-scanned" "$SUBJECT_REAL" \
    's/^            if (resolved, label) not in scan.raw_scanned:$/            if "attr" in regions and (resolved, label) not in scan.raw_scanned:/' -- bash "$SELF" --only include-r7
  mc_expect_red "M53 2: a plain include handed to a macro is not refused" "$SUBJECT_REAL" \
    's/^        if any(not std_trusted(m) for m in enclosing):$/        if False:/' -- bash "$SELF" --only include-r7,include-plain
  mc_expect_red "M54 2: a std macro name redefined or re-imported is still trusted" "$SUBJECT_REAL" \
    's/^        return not rebound(m)$/        return True/' -- bash "$SELF" --only include-r10,include-r11,include-plain
  mc_expect_red "M55 1d: a metavariable, a ! and a metavariable are not refused" "$SUBJECT_REAL" \
    's/ and (metavar_ends_at(k - 1) or (k >= 2 and words\[k - 1\] == "!" and metavar_ends_at(k - 2))):$/ and metavar_ends_at(k - 1):/' -- bash "$SELF" --only include-r9
  mc_expect_red "M56 B: doctests are not token-scanned" "$SUBJECT_REAL" \
    's/^    rust_doctest_scan(rs, _rs_doc_comment_texts(text) + doc_strings, scan, "raw_text", depth)$/    pass/' -- bash "$SELF" --only include-r9,include-r10,include-r11
  mc_expect_red "M57 B: a doc-capable string literal is not read as doc text" "$SUBJECT_REAL" \
    's/^            doc_strings.append(_rs_str_value(w))$/            pass/' -- bash "$SELF" --only include-r9,include-r10,include-r11
  mc_expect_red "M58 B: a block-context-ambiguous doc is not re-read unquoted" "$SUBJECT_REAL" \
    's/^    if doc_layout_ambiguous(text):$/    if False:/' -- bash "$SELF" --only include-r9
  mc_expect_red "M59 B: every fenced block is scanned as Rust, whatever its language" "$SUBJECT_REAL" \
    's/^                in_rust, cur = _fence_is_rust(entry\[1\]), \[\]$/                in_rust, cur = True, []/' -- bash "$SELF" --only include-r9,include-r10
  mc_expect_red "M60 B: rustdoc's hidden lines are scanned with their #" "$SUBJECT_REAL" \
    's/^        return t\[2:\]$/        return line/' -- bash "$SELF" --only include-r9
  mc_expect_red "M61 F5: an added or deleted text doctest reads non_executable" "$SUBJECT_REAL" \
    's/^        if markers and change.status != "M":$/        if False:/' -- bash "$SELF" --only paths-doctest,paths-wt25,paths-wt26,paths-pathspec
  mc_expect_red "M62 F5: a Sphinx doctest directive does not make an edit unknown" "$SUBJECT_REAL" \
    's/^        if "sphinx_doctest_directive" in markers or "wide_encoding" in markers:$/        if False:/' -- bash "$SELF" --only paths-doctest
  mc_expect_red "M63 B3: another file's rebinding of a trusted macro name is not read" "$SUBJECT_REAL" \
    's/^            tree_shadows.append(tree.std_macro_shadows())$/            tree_shadows.append((set(), False))/' -- bash "$SELF" --only include-r10,include-r11
  mc_expect_red "M64 B3: a glob use does not revoke trusted macro names" "$SUBJECT_REAL" \
    's/^                    revoke_all = True$/                    pass/' -- bash "$SELF" --only include-r10
  mc_expect_red "M65 B1: a Rust attribute before a language tag does not make the block Rust" "$SUBJECT_REAL" \
    's/^            if not seen_other:$/            if False:/' -- bash "$SELF" --only include-r10
  mc_expect_red "M67 B2: Sphinx directive names are matched case-sensitively" "$SUBJECT_REAL" \
    's/^                               re.IGNORECASE)$/                               0)/' -- bash "$SELF" --only paths-doctest
  mc_expect_red "M68 N1: a wide-encoded text is not decoded for markers" "$SUBJECT_REAL" \
    's/^            if SPHINX_DOCTEST_RE.search(text) or b">>>" in text:$/            if False:/' -- bash "$SELF" --only paths-doctest
  mc_expect_red "M69 G1: a binary-looking text carries no doctest marker" "$SUBJECT_REAL" \
    's/^    if blob is None:  # G1: never gated on is_binary$/    if blob is None or is_binary(blob):/' -- bash "$SELF" --only paths-doctest
  mc_expect_red "M70 B4: a symlink outside code_paths reads non_executable" "$SUBJECT_REAL" \
    's/^        if special:$/        if False:/' -- bash "$SELF" --only paths-doctest,paths-worktree,paths-wt25
  mc_expect_red "M71 B1/R11 F1: an info string with grammar beyond a word list ({..}, (..), quotes, escapes, entities) is not failed closed" "$SUBJECT_REAL" \
    's/^    if re.search(r"\[^A-Za-z0-9_.=:,\\s-\]", info):$/    if False:/' -- bash "$SELF" --only include-r10,include-r11
  mc_expect_red "M72 R11 F2: a whole-word macro_use elsewhere does not revoke trust" "$SUBJECT_REAL" \
    's/^                if re.search(r"\\bmacro_use\\b", t):$/                if False:/' -- bash "$SELF" --only include-r10,include-r11
  mc_expect_red "M73 R11 F2: only .rs files are searched for rebindings" "$SUBJECT_REAL" \
    's/whole_word=True, pathspec="\*")$/whole_word=True)/' -- bash "$SELF" --only include-r11
  mc_expect_red "M74 R11 F3: a decl_macro of a trusted name is not a rebinding" "$SUBJECT_REAL" \
    's/^    local_macros |= {words.k + 1. for k in range(n - 1) if words.k. == "macro".*$/    pass/' -- bash "$SELF" --only include-r11
  mc_expect_red "M75 R11 F4: an include of a tracked symlink is resolved as a plain file" "$SUBJECT_REAL" \
    's/^        if st == "symlink":$/        if False:/' -- bash "$SELF" --only include-link,include-resolve
  mc_expect_red "M76 R11 F4: a file a tracked link names is not aliased" "$SUBJECT_REAL" \
    's/^            hits.extend(h for h in symlink_aliases(trees\[side\], c.path) if h not in hits)$/            pass/' -- bash "$SELF" --only include-link,include-r14,include-resolve
  mc_expect_red "M77 R12 F1: a link on the way to .. is not seen" "$SUBJECT_REAL" \
    's/^        if prefix in links:$/        if False:/' -- bash "$SELF" --only include-link,include-resolve
  mc_expect_red "M78 R12 F1: .. out of an untracked directory is collapsed as a string" "$SUBJECT_REAL" \
    's/^            if here not in dirs:$/            if False:/' -- bash "$SELF" --only include-resolve
  mc_expect_red "M79 R12 F2: a raw identifier is read as r" "$SUBJECT_REAL" \
    's/^        if t == "r" and k + 2 < len(toks)/        if False and t == "r" and k + 2 < len(toks)/' -- bash "$SELF" --only include-r11
  mc_expect_red "M80 R13 F1/F2: a link target not followed exactly is not an alias" "$SUBJECT_REAL" \
    's/^            hits.append(link)  # not followed exactly: fail closed$/            pass/' -- bash "$SELF" --only include-link,include-r14,include-resolve
  mc_expect_red "M81 R12 F4: only the head side's links are read" "$SUBJECT_REAL" \
    's/^        for side in ("base", "head"):  # both sides: a retargeted link names two files$/        for side in ("head",):/' -- bash "$SELF" --only include-resolve
  mc_expect_red "M83 R14 F1: a link target this side does not track exactly is not an alias" "$SUBJECT_REAL" \
    's/^        elif resolved not in tree.paths() and resolved not in tree_dirs(tree):$/        elif False:/' -- bash "$SELF" --only include-r14
  mc_expect_red "M84 R14 F2: a backslash or drive spelling is followed as a relative path" "$SUBJECT_REAL" \
    's/^        if "\\0" in target or kind == "foreign":$/        if "\\0" in target:/' -- bash "$SELF" --only include-r14
  mc_expect_red "M85 R15 F2: a path with a case-fold or normalisation twin is resolved by its spelling" "$SUBJECT_REAL" \
    's/^        if fold_key(prefix) in twins:$/        if False:/' -- bash "$SELF" --only include-r14
  mc_expect_red "M87 R16 F1: the language-arm dispatch ignores a fold twin" "$SUBJECT_REAL" \
    's/^            if inside is not False or (lang is not None and lang != c.lang):$/            if inside is not False:/' -- bash "$SELF" --only paths-fold,paths-twin
  mc_expect_red "M89 R17 F1: the fold key keeps U+0131 apart from i" "$SUBJECT_REAL" \
    's/\.casefold()\.translate(_FOLD_EXTRA)$/.casefold()/' -- bash "$SELF" --only paths-fold
  # Not declared, because each is an equivalent mutant on this Unicode
  # version (round 19 enumerated the key's partition with and without it):
  # the first NFD, the NFD before `_iota_last`, and where the U+0131 map
  # sits. Nothing can redden them. The steps that do change a key are pinned.
  mc_expect_red "M90 R18 F1: the fold key normalises before casefold only" "$SUBJECT_REAL" \
    's/^    folded = unicodedata\.normalize("NFD", folded)\.casefold()/    folded = unicodedata.normalize("NFC", folded).casefold()/; /^    folded = _iota_last(unicodedata\.normalize("NFD", folded))$/d' -- bash "$SELF" --only paths-fold,paths-glob25
  mc_expect_red "M93 R19 F1: the fold key leaves each iota where D145 put it" "$SUBJECT_REAL" \
    's/^    folded = _iota_last(unicodedata\.normalize("NFD", folded))$/    folded = unicodedata.normalize("NFD", folded)/' -- bash "$SELF" --only paths-fold
  mc_expect_red "M94 R19 F1: an iota is not part of the run of marks around it" "$SUBJECT_REAL" \
    's/^        if ch == _IOTA or unicodedata\.combining(ch):$/        if unicodedata.combining(ch):/' -- bash "$SELF" --only paths-fold
  mc_expect_red "M95 R20 F1: the fold key keeps Default_Ignorable code points" "$SUBJECT_REAL" \
    's/^    folded = path\.translate(_DROP_IGNORABLE)$/    folded = path/' -- bash "$SELF" --only paths-fold
  mc_expect_red "M96 R20 F1: the fold key drops ignorables only after folding" "$SUBJECT_REAL" \
    's/^    folded = path\.translate(_DROP_IGNORABLE)$/    folded = path/; s/^    folded = _iota_last(unicodedata\.normalize("NFD", folded))$/    folded = _iota_last(unicodedata.normalize("NFD", folded)).translate(_DROP_IGNORABLE)/' -- bash "$SELF" --only paths-fold
  mc_expect_red "M97 R20 F2: the marks around an iota are left unsorted" "$SUBJECT_REAL" \
    's/^    return 256 if ch == _IOTA else unicodedata\.combining(ch)$/    return 256 if ch == _IOTA else 0/' -- bash "$SELF" --only paths-fold
  mc_expect_red "M98 R21 F1: an unassigned code point is judged by the fold key alone" "$SUBJECT_REAL" \
    's/^    if any(unicodedata\.category(ch) == "Cn" for ch in component):$/    if False:/' -- bash "$SELF" --only paths-foldwild,paths-resolver
  mc_expect_red "M99 R21 8.3: an NTFS short name is judged by its spelling" "$SUBJECT_REAL" \
    's/^    if _SHORT_NAME\.fullmatch(component\.rstrip("\. ")):$/    if False:/' -- bash "$SELF" --only paths-foldwild,paths-resolver,paths-twin
  mc_expect_red "M100 R21: an entry beside a wild name is judged by its own spelling" "$SUBJECT_REAL" \
    's/^            if w != prefix:$/            if False:/' -- bash "$SELF" --only paths-twin
  mc_expect_red "M101 R21: the resolver does not judge wild names on its walk" "$SUBJECT_REAL" \
    's/^        if wild_entry(tree, prefix):$/        if False:/' -- bash "$SELF" --only paths-twin
  mc_expect_red "M102 R22 F2: only a path's last level is judged for wild names" "$SUBJECT_REAL" \
    's/^    for i in range(1, len(parts) + 1):$/    for i in range(len(parts), len(parts) + 1):/' -- bash "$SELF" --only paths-foldwild,paths-resolver,paths-twin
  mc_expect_red "M103 R22 F1: the resolver takes its start directory as given" "$SUBJECT_REAL" \
    's|^    start_wild = wild_entry(tree, "/"\.join(parts)) if parts else None$|    start_wild = None|' -- bash "$SELF" --only paths-resolver
  mc_expect_red "M104 R22 F3: a short name's trailing dot hides it" "$SUBJECT_REAL" \
    's/fullmatch(component\.rstrip("\. "))/fullmatch(component)/' -- bash "$SELF" --only paths-foldwild
  mc_expect_red "M105 R22 F3: a short name's base may not hold a tilde" "$SUBJECT_REAL" \
    's|^_SHORT_NAME = re\.compile(r"\[^\./\]{1,6}|_SHORT_NAME = re.compile(r"[^.~/]{1,6}|' -- bash "$SELF" --only paths-foldwild,paths-resolver
  mc_expect_red "M106 R23 F1: an untracked nested repository is read as a file" "$SUBJECT_REAL" \
    's|^            if path\.endswith("/"):$|            if False:|' -- bash "$SELF" --only paths-worktree
  mc_expect_red "M107 R23 F2: a code_paths glob matches only its exact spelling" "$SUBJECT_REAL" \
    's/^        if any(m\.fullmatch(fold_key(path)) for m in self\.fold_matchers):$/        if False:/' -- bash "$SELF" --only paths-fold,paths-resolver,paths-glob24,paths-glob25
  mc_expect_red "M108 R23 F2: a wild literal in a code_paths glob is trusted" "$SUBJECT_REAL" \
    's/^        if wild is not None:$/        if False:/' -- bash "$SELF" --only paths-foldwild,paths-resolver,paths-glob24,paths-glob25,paths-glob26
  mc_expect_red "M109 R23 F3: a name that is not UTF-8 is judged by its spelling" "$SUBJECT_REAL" \
    's/^    if any(unicodedata\.category(ch) == "Cs" for ch in component):$/    if False:/' -- bash "$SELF" --only paths-twin
  mc_expect_red "M110 R24 F1: a folded glob reads \`?\` and a class as one character" "$SUBJECT_REAL" \
    's/^            segs.append("\.\*")   # ? and a class.*$/            segs.append(".")/' -- bash "$SELF" --only paths-fold,paths-glob24
  mc_expect_red "M111 R24 F2: the wild check reads a glob before brace expansion" "$SUBJECT_REAL" \
    's/^        for alt in alts:   # the wild check runs on every alternative (round 24)$/        for alt in [tokens]:/' -- bash "$SELF" --only paths-glob24,paths-glob26
  mc_expect_red "M112 R24 F2: a glob component with wildcards is trusted although it holds a tilde" "$SUBJECT_REAL" \
    's/^    return "~" in lits or /    return False and "~" in lits or /' -- bash "$SELF" --only paths-glob25,paths-glob26
  mc_expect_red "M113 R24 F3: a \`/**/\` in a folded glob must stand for no directory as well" "$SUBJECT_REAL" \
    's|^            segs.append("(?:/\|/\.\*/)")$|            segs.append("/.*/")|' -- bash "$SELF" --only paths-glob24
  mc_expect_red "M114 R24 F4: a deleted path is never checked on disk" "$SUBJECT_REAL" \
    's/^        changes += \[r for r in (reconcile_deleted(root, c, listed) for c in changes if c\.status == "D") if r\]$/        pass/' -- bash "$SELF" --only paths-worktree,paths-wt25
  mc_expect_red "M115 R24 F6: an ancestor twin's reason wins over the whole path's" "$SUBJECT_REAL" \
    's/^    for i in range(len(parts), 0, -1):   # the whole path first: its reason wins$/    for i in range(1, len(parts) + 1):/' -- bash "$SELF" --only paths-twin
  mc_expect_red "M116 R25 F2: a \`/**\` after a \`/**/\` keeps the \`/**/\`" "$SUBJECT_REAL" \
    's/branches\[-1\]\.append(last if last in (("rprefix",), ("rsuffix",)) else/branches[-1].append(last if last[0] in ("rprefix", "rsuffix", "rzom") else/' -- bash "$SELF" --only paths-glob25
  mc_expect_red "M117 R25 F3: a \`**\` after a leading \`**/\` is a \`/**/\`" "$SUBJECT_REAL" \
    's/branches\[-1\]\.append(last if last in (("rprefix",), ("rsuffix",)) else/branches[-1].append(last if last == ("rsuffix",) else/' -- bash "$SELF" --only paths-glob25
  mc_expect_red "M118 R25 F2: a class cannot hold \`/\`" "$SUBJECT_REAL" \
    's/^            if c is None:$/            if c is None or c == "\/":/' -- bash "$SELF" --only paths-glob25,paths-glob26
  mc_expect_red "M119 R25 F2: a class inside braces is not skipped" "$SUBJECT_REAL" \
    's/^        elif c == "\[":$/        elif c == "[" and not alt_starts:/' -- bash "$SELF" --only paths-glob25
  mc_expect_red "M120 R25 F1: a file left on disk wins over the deletion" "$SUBJECT_REAL" \
    's/^            if rank\[c\.verdict\] > rank\[kept\[-1\]\.verdict\]:$/            if True:/' -- bash "$SELF" --only paths-wt25,paths-wt26,paths-pathspec
  mc_expect_red "M121 R25 F1: an ignored file left on disk replaces the deletion" "$SUBJECT_REAL" \
    's/^    if change\.path not in listed:$/    if False:/' -- bash "$SELF" --only paths-wt25
  mc_expect_red "M122 R25 F1: a path under a link is looked up through it" "$SUBJECT_REAL" \
    's/^    if any(os\.path\.islink(os\.path\.join(root, \*parts\[:i\])) for i in range(1, len(parts))):$/    if False:/' -- bash "$SELF" --only paths-wt25
  mc_expect_red "M123 R25 F6: a link replacing a file is followed" "$SUBJECT_REAL" \
    's/^    if os\.path\.islink(full):$/    if False:/' -- bash "$SELF" --only paths-wt25
  mc_expect_red "M124 R25 F4: a \`?\` cannot supply an 8.3 name's \`~\`" "$SUBJECT_REAL" \
    's/^    return _SN_KINDS if t\[0\] == "any" else _SN_KINDS - {"~"}$/    return _SN_KINDS - {"~"}/' -- bash "$SELF" --only paths-glob25,paths-glob26
  mc_expect_red "M125 R25 F4: a class cannot supply an 8.3 name's \`~\`" "$SUBJECT_REAL" \
    's/if cls\.fullmatch(bytes(\[b\])))$/if cls.fullmatch(bytes([b])) and b != 0x7e)/' -- bash "$SELF" --only paths-glob25,paths-glob26
  mc_expect_red "M126 R25 F5: marks at a literal's edge stay in the folded glob" "$SUBJECT_REAL" \
    's/^            if k and segs\[k - 1\] in ("\.\*", "\/\.\*"):$/            if False:/' -- bash "$SELF" --only paths-fold,paths-glob25
  mc_expect_red "M127 R25: a link git does not track yet aliases nothing" "$SUBJECT_REAL" \
    's/^                    if name and os\.path\.islink(full):$/                    if False:/' -- bash "$SELF" --only paths-wt25
  mc_expect_red "M128 R26 F1: an untracked add is not read" "$SUBJECT_REAL" \
    's/^            elif c\.untracked or c\.on_disk or c\.status == "T":$/            elif c.on_disk or c.status == "T":/' -- bash "$SELF" --only paths-wt26
  mc_expect_red "M129 R26 F1: a staged add is not rebuilt from the working tree" "$SUBJECT_REAL" \
    's/^            elif c\.status in ("M", "A"):   # tracked, staged or not, its mode changed or not$/            elif c.status == "M":/' -- bash "$SELF" --only paths-wt26,paths-pathspec
  mc_expect_red "M130 R26 F1: a mode change is not rebuilt from the working tree" "$SUBJECT_REAL" \
    's/^            elif c\.status in ("M", "A"):   # tracked, staged or not, its mode changed or not$/            elif c.status in ("M", "A") and c.old_mode == c.new_mode:/' -- bash "$SELF" --only paths-wt26,paths-pathspec
  mc_expect_red "M131 R26 F1: only the first 256 bytes of a file on disk are read" "$SUBJECT_REAL" \
    's/^                    data = fh\.read()$/                    data = fh.read(256)/' -- bash "$SELF" --only paths-wt26
  mc_expect_red "M132 R26 F1: a commit of the index is not judged" "$SUBJECT_REAL" \
    's/^            if (s\.path, s\.status, s\.new_mode, s\.new_sha) not in sides:   # the index holds another side$/            if False:/' -- bash "$SELF" --only paths-wt26,paths-attr
  mc_expect_red "M133 R26 F2: a \`**/\` past the start ends the 8.3 component" "$SUBJECT_REAL" \
    's/^            confs = confs | {("start", k + 1)}$/            confs = {("start", k + 1)}/' -- bash "$SELF" --only paths-glob26
  mc_expect_red "M134 R26 F2: a \`?\` or class cannot supply the \`/\` before an 8.3 name" "$SUBJECT_REAL" \
    's/^            if "\/" in kinds:   # the `\/` that ends the component$/            if False:/' -- bash "$SELF" --only paths-glob26
  mc_expect_red "M135 R26 F2: a \`*\` cannot supply the \`/\` before an 8.3 name" "$SUBJECT_REAL" \
    's/^            confs = confs | _sn_closure({("start", k + 1)}, free)$/            confs = confs/' -- bash "$SELF" --only paths-glob26
  mc_expect_red "M136 R26 F2: the run after \`/**\` does not begin the next 8.3 component" "$SUBJECT_REAL" \
    's/^            confs = _sn_closure({("start", k + 1)}, free)$/            confs = {("start", k + 1)}/' -- bash "$SELF" --only paths-glob26
  mc_expect_red "M137 R26 F2: the literal check's components end at a \`**/\` past the start" "$SUBJECT_REAL" \
    's/^        if t == ("lit", "\/") or t\[0\] in ("rsuffix", "rzom") or (t\[0\] == "rprefix" and not k):$/        if t == ("lit", "\/") or t[0] in ("rsuffix", "rzom", "rprefix"):/' -- bash "$SELF" --only paths-glob26
  mc_expect_red "M138 R26 F3: a glob is trimmed as Python trims it" "$SUBJECT_REAL" \
    's/^            g = g\.strip(_RUST_WHITESPACE)$/            g = g.strip()/' -- bash "$SELF" --only paths-glob26
  mc_expect_red "M139 R27 F1: a repository path is read as pathspec magic" "$SUBJECT_REAL" \
    's/^        cmd\.append("--literal-pathspecs")$/        pass/' -- bash "$SELF" --only paths-pathspec
  mc_expect_red "M140 R27 F1: an inherited git scope (GIT_ICASE_PATHSPECS among it) is kept" "$SUBJECT_REAL" \
    's/^    if not git_scope_strip():$/    if False:/' -- bash "$SELF" --only paths-pathspec
  # M141 (R27 F2: a file off disk stored with no conversion) is retired since
  # round 28 as an equivalent mutant: every attribute that converts on the
  # way in now reads unknown before the file is stored, and what is left,
  # core.autocrlf's line ends, rewrites only a file read off disk because it
  # was added, type-changed or left by `git rm --cached`, whose added or
  # deleted side decides the verdict whatever its line ends are.
  mc_expect_red "M142 R27 F3: a checkout attribute's reason is not applied" "$SUBJECT_REAL" \
    's/^        if c\.path in attributed:   # .*$/        if False:/' -- bash "$SELF" --only paths-pathspec,paths-attr,paths-attrlocal,paths-attrsys
  # M143 (R27: only the head side's attributes read) is retired since round
  # 28 as an equivalent mutant: a side whose attributes differ from another's
  # needs a changed `.gitattributes`, which makes every path unknown first.
  mc_expect_red "M144 R27 F4: no 8.3 hit before a \`**/\` past the start" "$SUBJECT_REAL" \
    's/^            ends(_sn_closure(confs, free))$/            pass/' -- bash "$SELF" --only paths-glob26
  mc_expect_red "M145 R27 F4: a \`?\` or class is never one byte of a longer character" "$SUBJECT_REAL" \
    's/^            nxt = _sn_step(confs, kinds) | confs$/            nxt = _sn_step(confs, kinds)/' -- bash "$SELF" --only paths-glob26
  mc_expect_red "M146 R28 F3: only the filter attribute changes a checkout" "$SUBJECT_REAL" \
    's/^CHECKOUT_ATTRS = (.*)$/CHECKOUT_ATTRS = ("filter",)/' -- bash "$SELF" --only paths-pathspec,paths-attr,paths-attrlocal
  mc_expect_red "M147 R28: attributes that differ between sides, none set, are not unknown" "$SUBJECT_REAL" \
    's/^            if len(set(values)) > 1:$/            if False:/' -- bash "$SELF" --only paths-attrlocal
  mc_expect_red "M148 R28 F2: a changed .gitattributes leaves every other path judged" "$SUBJECT_REAL" \
    's/^    if source:$/    if False:/' -- bash "$SELF" --only paths-pathspec,paths-attr,paths-attrlocal,paths-attrsys
  mc_expect_red "M149 R28: a local attributes source is not read" "$SUBJECT_REAL" \
    's/^    if local:$/    if False:/' -- bash "$SELF" --only paths-attrlocal,paths-attrsys
  mc_expect_red "M150 R28: the working tree's attributes are not read" "$SUBJECT_REAL" \
    's/^        sides\.append(read_attributes(root, paths))$/        pass/' -- bash "$SELF" --only paths-attrlocal
  # The real index's read (`cached=True`, F4) is not declared: the index's
  # attributes differ from the base's only where a staged `.gitattributes`
  # makes every path unknown first, so dropping it is an equivalent mutant.
  mc_expect_red "M151 R28 F5: an inherited GIT_ATTR_SOURCE is honoured" "$SUBJECT_REAL" \
    's/^ATTR_ENV = ("GIT_ATTR_SOURCE",)$/ATTR_ENV = ()/' -- bash "$SELF" --only paths-attrlocal
  mc_expect_red "M152 R28 F5: attr.tree is honoured" "$SUBJECT_REAL" \
    's/^GIT_PIN = ("--no-replace-objects", "-c", "attr.tree=")$/GIT_PIN = ("--no-replace-objects",)/' -- bash "$SELF" --only paths-attrlocal
  mc_expect_red "M153 R28 F6: replace refs stand in for objects" "$SUBJECT_REAL" \
    's/^GIT_PIN = ("--no-replace-objects", "-c", "attr.tree=")$/GIT_PIN = ("-c", "attr.tree=")/' -- bash "$SELF" --only paths-attrlocal
  mc_expect_red "M154 R28 F7: a commit's attributes are read from the checkout" "$SUBJECT_REAL" \
    's/^            args\.append("--cached")$/            pass/' -- bash "$SELF" --only paths-attrlocal
  mc_expect_red "M155 R29 F2: only the exact spelling \`.gitattributes\` is an attributes source" "$SUBJECT_REAL" \
    's/^        if fold_key(posixpath\.basename(c\.path)) == key:$/        if posixpath.basename(c.path) == ".gitattributes":/' -- bash "$SELF" --only paths-attrsys
  mc_expect_red "M159 R31 F1: an attribute line is read as a comment after any whitespace" "$SUBJECT_REAL" \
    's/^        s = ln\.lstrip(b" \\t\\r\\n")$/        s = ln.strip()/' -- bash "$SELF" --only paths-attrsys
  mc_expect_red "M160 R31: a path git prints is stripped of trailing whitespace" "$SUBJECT_REAL" \
    's/^    return out\.decode("utf-8", "surrogateescape")$/    return out.decode("utf-8", "surrogateescape").strip()/' -- bash "$SELF" --only paths-attrsys,paths-root
  mc_expect_red "M161 R31: a comment-only attribute file is a source" "$SUBJECT_REAL" \
    's/^        if s and not s\.startswith(b"#"):$/        if s:/' -- bash "$SELF" --only paths-attrsys
  mc_expect_red "M162 R32: the repo root git prints is stripped of whitespace" "$SUBJECT_REAL" \
    's/^    root = _git_path_line(git(args\.repo_root, "rev-parse", "--show-toplevel"))$/    root = _git_path_line(git(args.repo_root, "rev-parse", "--show-toplevel")).strip()/' -- bash "$SELF" --only paths-root
  # M163-M262 drive the tree-sitter arms (rounds 33-53), so they are declared
  # only with the grammar pack: without it their narrowed sections assert
  # nothing, and a re-run that exits 1 for that reason is not a kill.
  mutants_ts_rounds_33_53() {
    mc_expect_red "M163 R33 F1: bash's blanks are the grammar's" "$SUBJECT_REAL" \
      's/^LANG_BLANKS = {"bash": b" \\t\\n", /LANG_BLANKS = {/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M164 R33 F2: an evaluator under a wrapper is not found" "$SUBJECT_REAL" \
      's/^            at = next((i for i, n in enumerate(names) if i and (n in SHELL_EVALUATORS or n in SHELL_INTERPRETERS)), None)$/            at = None/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M165 R33 F1: every tree-sitter comment is a bash comment" "$SUBJECT_REAL" \
      's/^            if lang == "bash" and not shell_comment_is_comment(node, src):$/            if False:/' -- bash "$SELF" --only shell-r33,shell-r35
    mc_expect_red "M166 R33 F1: a # after any byte starts a comment" "$SUBJECT_REAL" \
      's/^        if src\[start - 1:start\] not in SHELL_WORD_BOUNDARY:$/        if False:/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M167 R33 F1: an escaped boundary byte is a boundary" "$SUBJECT_REAL" \
      's/^        if slashes % 2:$/        if False:/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M168 R33 F4: a # inside arithmetic is a comment" "$SUBJECT_REAL" \
      's/^        if parent.type in SHELL_NO_COMMENT_CONTEXTS:$/        if False:/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M169 R33 F2: evaluated text that does not parse is not refused" "$SUBJECT_REAL" \
      's/^        if tree.root_node.has_error:$/        if False:/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M170 R33 F2: /proc and /dev/fd paths do not name the script" "$SUBJECT_REAL" \
      's/^SHELL_SELF_PATHS = (b"\/proc\/", b"\/dev\/fd\/")$/SHELL_SELF_PATHS = ()/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M171 R33 F2: a literal naming the file does not name the script" "$SUBJECT_REAL" \
      's/ or (own and own in text)):$/):/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M172 R33 F2: \$_ does not name the script" "$SUBJECT_REAL" \
      's/^SHELL_SELF_SPECIALS = {b"0", b"_"}$/SHELL_SELF_SPECIALS = {b"0"}/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M173 R33 F2: BASH_ARGV0 does not name the script" "$SUBJECT_REAL" \
      's/^SHELL_SELF_VARIABLES = {b"BASH_SOURCE", b"BASH_ARGV0", b"BASH_ARGV"}$/SHELL_SELF_VARIABLES = {b"BASH_SOURCE"}/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M174 R33 F2: an indirect expansion does not name the script" "$SUBJECT_REAL" \
      's/^            if len(kids) > 1 and kids\[1\] == "!":$/            if False:/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M175 R33 F2: a substring of \$@ does not name the script" "$SUBJECT_REAL" \
      's/^                  and ":" in kids):$/                  and False):/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M176 R33 F2: caller does not name the script" "$SUBJECT_REAL" \
      's/ == b"caller":$/ == b"caller-":/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M177 R33 F2: evaluated text is not read" "$SUBJECT_REAL" \
      's/^        texts = shell_evaluated_texts(tree, work)$/        texts = []/' -- bash "$SELF" --only shell-r33,shell-r35
    mc_expect_red "M178 R33 F2: computed evaluated text does not refuse" "$SUBJECT_REAL" \
      's/^            if lit is None:$/            if False:/' -- bash "$SELF" --only shell-r33,shell-r35
    mc_expect_red "M179 R33 F2: a computed command name is not a reader" "$SUBJECT_REAL" \
      's/^    if name is None:  # a computed command name$/    if False:/' -- bash "$SELF" --only shell-r33,shell-r35
    mc_expect_red "M180 R33 F2: cp is not a reader" "$SUBJECT_REAL" \
      's/"readarray", "cp", "install"/"readarray", "install"/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M181 R33 F2: a wrapper handed a computed word is not a reader" "$SUBJECT_REAL" \
      's/^            if lit is None:  # a computed word under a wrapper$/            if False:/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M182 R33 F2: an alias's text keeps its name" "$SUBJECT_REAL" \
      's/^                lit = lit.split(b"=", 1)\[1\] if b"=" in lit else b""$/                pass/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M183 R33 F2: a shell's -c option is not found" "$SUBJECT_REAL" \
      's/^            opt = next((i for i, n in enumerate(names\[at + 1:\]) if n.startswith("-") and not n.startswith("--") and "c" in n), None)$/            opt = None/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M184 R33 F3: a function's source text is not compared" "$SUBJECT_REAL" \
      's/^    if lang in JS_FAMILY and base != head and js_source_texts(tb, base) != js_source_texts(th, head):$/    if False:/' -- bash "$SELF" --only ts-self
    mc_expect_red "M185 R33 F3: a class is not a source-text node" "$SUBJECT_REAL" \
      's/"class_declaration", "class", "abstract_class_declaration",/"abstract_class_declaration",/' -- bash "$SELF" --only ts-self
    mc_expect_red "M186 R33 F3: __filename does not name the module" "$SUBJECT_REAL" \
      's/^JS_SELF_IDENTIFIERS = {b"__filename", b"__dirname"}$/JS_SELF_IDENTIFIERS = set()/' -- bash "$SELF" --only ts-self,ts-escape
    mc_expect_red "M187 R33 F3: import.meta does not name the module" "$SUBJECT_REAL" \
      's/^            if kind == "meta_property":$/            if False:/' -- bash "$SELF" --only ts-self
    mc_expect_red "M188 R33 F3: a TS file is not named by its emitted name" "$SUBJECT_REAL" \
      's/^        names.update(js_stem(path) + ext for ext in JS_EMIT_EXTS)$/        pass/' -- bash "$SELF" --only ts-self
    mc_expect_red "M189 R33 F3: file! inside a macro's tokens is not seen" "$SUBJECT_REAL" \
      's/^            if (node.child_count == 0 and text in RUST_SELF_MACROS and node.next_sibling is not None$/            if (False and node.next_sibling is not None/' -- bash "$SELF" --only rust
    mc_expect_red "M190 R33 F3: CARGO_MANIFEST_DIR does not name the file" "$SUBJECT_REAL" \
      's/^RUST_SELF_NAMES = (b"CARGO_MANIFEST_DIR", b"Location")$/RUST_SELF_NAMES = ()/' -- bash "$SELF" --only rust
    mc_expect_red "M191 R34 F1: a C-style for header reads # as a comment" "$SUBJECT_REAL" \
      's/^        if parent.type == "c_style_for_statement":$/        if False:/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M192 R34 F1: a \${ } expansion reads # as a comment" "$SUBJECT_REAL" \
      's/^SHELL_NO_COMMENT_CONTEXTS = {"arithmetic_expansion", "expansion", "subscript"}$/SHELL_NO_COMMENT_CONTEXTS = {"arithmetic_expansion", "subscript"}/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M193 R34 F1: a subscript reads # as a comment" "$SUBJECT_REAL" \
      's/^SHELL_NO_COMMENT_CONTEXTS = {"arithmetic_expansion", "expansion", "subscript"}$/SHELL_NO_COMMENT_CONTEXTS = {"arithmetic_expansion", "expansion"}/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M194 R34 F1: a body or command substitution does not start a fresh command line" "$SUBJECT_REAL" \
      's/^SHELL_COMMENT_BARRIERS = {"command_substitution", "process_substitution", "subshell", "do_group",$/SHELL_COMMENT_BARRIERS = {"x",/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M195 R34 F2: evaluated words are read only joined" "$SUBJECT_REAL" \
      's/^        if len(parts) > 1:$/        if False:/' -- bash "$SELF" --only shell-r33
    mc_expect_red "M196 R34 F3: a JS name is stemmed at its first dot" "$SUBJECT_REAL" \
      's/^    return root if ext in JS_EMIT_EXTS and root else base$/    return base.split(".", 1)[0]/' -- bash "$SELF" --only ts-self
    mc_expect_red "M197 R34 F3: an extensionless specifier does not name the module" "$SUBJECT_REAL" \
      's/^                if js_names_specifier(reading, specifiers):$/                if False:/' -- bash "$SELF" --only ts-self,ts-jsx2
    mc_expect_red "M198 R34 F3: a directory specifier does not name its index module" "$SUBJECT_REAL" \
      's/^    if stem == "index":$/    if False:/' -- bash "$SELF" --only ts-self,ts-jsx2
    mc_expect_red "M199 R34 F4: callee and caller do not reach the wrapper" "$SUBJECT_REAL" \
      's/^JS_SELF_PROPERTIES = {b"stack", b"currentScript", b"callee", b"caller"}$/JS_SELF_PROPERTIES = {b"stack", b"currentScript"}/' -- bash "$SELF" --only ts-self,ts-escape
    mc_expect_red "M200 R34 F4: module-level arguments are not the wrapper's" "$SUBJECT_REAL" \
      's/^                if up is None:$/                if False:/' -- bash "$SELF" --only ts-self,ts-jsx
    mc_expect_red "M201 R34 F4: require.cache does not hold the module's path" "$SUBJECT_REAL" \
      's/(b"require", b"main"), (b"require", b"cache"),$/(b"require", b"main"),/' -- bash "$SELF" --only ts-self
    mc_expect_red "M202 R34 F4: a function's own arguments read as the wrapper's" "$SUBJECT_REAL" \
      's/^    "function_declaration", "function_expression", "function", "method_definition",$/    "x",/' -- bash "$SELF" --only ts-self
    mc_expect_red "M203 R35 F1: an array's [ ] key is not checked" "$SUBJECT_REAL" \
      's/^    if node.parent is not None and node.parent.type == "array" and shell_array_key_open(node, src):$/    if False:/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M204 R35 F1: an array key's open bracket is not counted" "$SUBJECT_REAL" \
      's/^        depth = max(0, depth + text.count(b"\[") - text.count(b"\]"))$/        depth = 0/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M205 R35 F5: a comment after a C-style for header is arithmetic" "$SUBJECT_REAL" \
      's/^            if close is None or node.start_byte < close.start_byte:$/            if True:/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M206 R35 F2: verbose mode is not seen" "$SUBJECT_REAL" \
      's/^        if shell_echoes_source(tree, work):$/        if False:/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M207 R35 F2: a shebang -v flag is not seen" "$SUBJECT_REAL" \
      's/^            if b"verbose" in tok or (tok.startswith(b"-") and not tok.startswith(b"--") and b"v" in tok):$/            if False:/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M208 R35 F2: set -v is not seen" "$SUBJECT_REAL" \
      's/^            if lit == b"verbose" or (lit.startswith(b"-") and b"v" in lit\[1:\]):$/            if lit == b"verbose":/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M209 R35 F2: set -o verbose is not seen" "$SUBJECT_REAL" \
      's/^            if lit == b"verbose" or (lit.startswith(b"-") and b"v" in lit\[1:\]):$/            if lit.startswith(b"-") and b"v" in lit[1:]:/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M210 R35 F2: a wrapped set is not seen" "$SUBJECT_REAL" \
      's/^        at = 0 if names\[0\] in ("set", "shopt") else next($/        at = 0 if names[0] in ("set", "shopt") else None if True else next(/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M211 R35 F2: a computed set option word is not seen" "$SUBJECT_REAL" \
      's/^            if lit is None:  # an option word computed at run time$/            if False:/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M212 R35 F3: a bracket property read is not seen" "$SUBJECT_REAL" \
      's/^    if kind in ("member_expression", "subscript_expression"):$/    if kind == "member_expression":/' -- bash "$SELF" --only ts-self,ts-escape
    mc_expect_red "M213 R35 F3: a destructured property is not seen" "$SUBJECT_REAL" \
      's/^    elif kind == "variable_declarator" or kind == "assignment_expression":$/    elif False:/' -- bash "$SELF" --only ts-self
    mc_expect_red "M214 R35 F3: a computed property of module is not seen" "$SUBJECT_REAL" \
      's/^        if name is not None or otext in selfobjs:$/        if name is not None:/' -- bash "$SELF" --only ts-self
    mc_expect_red "M215 R35 F3: a computed-key string is not read as a name" "$SUBJECT_REAL" \
      's/^    if node.type == "computed_property_name":$/    if False:/' -- bash "$SELF" --only ts-self
    mc_expect_red "M216 R35 F3: a variable index reads as a static name" "$SUBJECT_REAL" \
      's/^    if node.type in ("property_identifier", "shorthand_property_identifier_pattern", "private_property_identifier"):$/    if node.type in ("property_identifier", "identifier", "shorthand_property_identifier_pattern", "private_property_identifier"):/' -- bash "$SELF" --only ts-self
    mc_expect_red "M217 R36: a shebang long option is not a verbose flag" "$SUBJECT_REAL" \
      's/^            if b"verbose" in tok or (tok.startswith(b"-") and not tok.startswith(b"--") and b"v" in tok):$/            if tok == b"verbose" or (tok.startswith(b"-") and not tok.startswith(b"--") and b"v" in tok):/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M218 R36: only some node types count as a command's words" "$SUBJECT_REAL" \
      's/^    return \[kids\[at\]\] + \[ch for ch in kids\[at + 1:\] if ch.type not in SHELL_NON_WORDS\]$/    return [kids[at]] + [ch for ch in kids[at + 1:] if ch.type in ("word", "string", "raw_string", "concatenation", "simple_expansion", "expansion", "command_substitution")]/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M219 R36: ANSI-C quoting is not decoded" "$SUBJECT_REAL" \
      's/^        if c == b"\$" and word\[i + 1:i + 2\] == b"." and ansi:$/        if False:/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M220 R36: array-key brackets are counted inside quotes" "$SUBJECT_REAL" \
      's/^        text = shell_unquoted_bytes(ch, src)$/        text = src[ch.start_byte:ch.end_byte]/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M221 R36: an escaped bracket closes an array key" "$SUBJECT_REAL" \
      's/^        return re.sub(rb"\\\\.", b"", src\[node.start_byte:node.end_byte\], flags=re.S)$/        return src[node.start_byte:node.end_byte]/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M222 R36: an arithmetic expansion reads as a literal" "$SUBJECT_REAL" \
      's/^SHELL_EXPANSIONS = {"simple_expansion", "expansion", "command_substitution", "arithmetic_expansion",$/SHELL_EXPANSIONS = {"simple_expansion", "expansion", "command_substitution",/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M223 R36: a string key is not read by its value" "$SUBJECT_REAL" \
      's/^        return js_string_value(node, src)$/        return None/' -- bash "$SELF" --only ts-self,ts-escape
    mc_expect_red "M224 R36: a \\u escape is not decoded" "$SUBJECT_REAL" \
      's/^            elif body\[:1\] == b"u" and len(body) == 5:$/            elif False:/' -- bash "$SELF" --only ts-self,ts-escape
    mc_expect_red "M225 R36: a \\x escape is not decoded" "$SUBJECT_REAL" \
      's/^            elif body\[:1\] == b"x" and len(body) == 3:$/            elif False:/' -- bash "$SELF" --only ts-self
    mc_expect_red "M226 R36: parentheses hide the object" "$SUBJECT_REAL" \
      's/^        if kind == "parenthesized_expression" and node.named_child_count:$/        if False:/' -- bash "$SELF" --only ts-self
    mc_expect_red "M227 R36: a comma operator hides the object" "$SUBJECT_REAL" \
      's/^        elif kind == "sequence_expression" and node.named_child_count:$/        elif False:/' -- bash "$SELF" --only ts-self
    mc_expect_red "M228 R36: a TS assertion hides the object" "$SUBJECT_REAL" \
      's/^        elif kind in ("as_expression", "satisfies_expression", "non_null_expression") and node.named_child_count:$/        elif False:/' -- bash "$SELF" --only ts-self
    mc_expect_red "M229 R36: a globalThis prefix hides the global" "$SUBJECT_REAL" \
      's/^            if obj is not None and key is not None and js_object_name(obj, src) in JS_GLOBAL_OBJECTS:$/            if False:/' -- bash "$SELF" --only ts-self
    mc_expect_red "M230 R36: a decoded literal is not checked for the file's name" "$SUBJECT_REAL" \
      's/^                hit = next((n for n in names if n in reading), None)$/                hit = None/' -- bash "$SELF" --only ts-self,ts-escape,ts-jsx,ts-jsx2
    mc_expect_red "M231 R36: a plain template key is not a static name" "$SUBJECT_REAL" \
      's/^    if node.type in ("string", "template_string"):$/    if node.type == "string":/' -- bash "$SELF" --only ts-self
    mc_expect_red "M232 R37: a bash word is not cut at its first NUL" "$SUBJECT_REAL" \
      's/^    return None if lit is None else lit.split(b"\\0", 1)\[0\]$/    return lit/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M233 R37: a pattern word reads as a literal" "$SUBJECT_REAL" \
      's/^    if shell_has_expansion(node) or shell_has_glob(node, src):$/    if shell_has_expansion(node):/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M234 R37: a [ ] pattern is not a glob" "$SUBJECT_REAL" \
      's/^    if k >= 0 and b"\]" in text\[k + 1:\]:$/    if False:/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M235 R37: a brace expansion is not a pattern" "$SUBJECT_REAL" \
      's/^    return k >= 0 and (b"," in text\[k + 1:\] or b"\.\." in text\[k + 1:\])$/    return False/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M236 R37: a JS legacy octal escape is not decoded" "$SUBJECT_REAL" \
      's/^            if octal:$/            if False:/' -- bash "$SELF" --only ts-escape
    mc_expect_red "M237 R38: digits past a JS octal escape are dropped" "$SUBJECT_REAL" \
      's/^                out.append(body\[octal.end():\].decode("utf-8", "replace"))$/                pass/' -- bash "$SELF" --only ts-escape
    mc_expect_red "M238 R38: a brace_expression's pieces are not unquoted bytes" "$SUBJECT_REAL" \
      's/^    if node.type == "word" or node.child_count == 0:$/    if node.type == "word":/' -- bash "$SELF" --only shell-r35
    mc_expect_red "M239 R39: a JS \\u surrogate pair is not joined" "$SUBJECT_REAL" \
      's/^    text = "".join(out).encode("utf-16-le", "surrogatepass").decode("utf-16-le", "replace")$/    text = "".join(out)/' -- bash "$SELF" --only ts-escape
    mc_expect_red "M240 R40: a lone JS surrogate is kept" "$SUBJECT_REAL" \
      's/^    text = "".join(out).encode("utf-16-le", "surrogatepass").decode("utf-16-le", "replace")$/    text = "".join(out).encode("utf-16-le", "surrogatepass").decode("utf-16-le", "surrogatepass")/' -- bash "$SELF" --only ts-escape
    mc_expect_red "M241 R40: a raw non-UTF-8 byte is kept as a surrogate escape" "$SUBJECT_REAL" \
      's/^            out.append(text.decode("utf-8", "replace"))$/            out.append(text.decode("utf-8", "surrogateescape"))/' -- bash "$SELF" --only ts-escape
    mc_expect_red "M242 R41: a JS identifier is read with its escapes kept" "$SUBJECT_REAL" \
      's/^    if b"\\\\" not in text:$/    if True:/' -- bash "$SELF" --only ts-escape,ts-jsx
    mc_expect_red "M243 R41: a template keeps a CR" "$SUBJECT_REAL" \
      's/^                text = text.replace(b"\\r\\n", b"\\n").replace(b"\\r", b"\\n")$/                pass/' -- bash "$SELF" --only ts-escape
    mc_expect_red "M244 R42: a JSX character reference is not decoded" "$SUBJECT_REAL" \
      's/^    if node.type == "string" and node.parent is not None and node.parent.type == "jsx_attribute":$/    if False:/' -- bash "$SELF" --only ts-jsx,ts-jsx2
    mc_expect_red "M245 R42: JSX text is not read" "$SUBJECT_REAL" \
      's/^            if value is None and kind in ("jsx_element", "jsx_fragment"):$/            if False:/' -- bash "$SELF" --only ts-jsx,ts-jsx2
    mc_expect_red "M246 R42: a shorthand arguments is not seen" "$SUBJECT_REAL" \
      's/^            if kind in ("identifier", "shorthand_property_identifier") and js_identifier_name(node, src) == b"arguments":$/            if kind == "identifier" and js_identifier_name(node, src) == b"arguments":/' -- bash "$SELF" --only ts-jsx
    mc_expect_red "M247 R43: a named JSX reference is not decoded" "$SUBJECT_REAL" \
      's/^        code = html.entities.name2codepoint.get(ref, 0x27 if ref == "apos" else None)$/        code = None/' -- bash "$SELF" --only ts-jsx
    mc_expect_red "M248 R43: a hex JSX reference is not decoded" "$SUBJECT_REAL" \
      's/^            if ref\[:2\] == "#x":$/            if False:/' -- bash "$SELF" --only ts-jsx
    mc_expect_red "M249 R43: JSX text is not read as the compilers evaluate it" "$SUBJECT_REAL" \
      's/^        for _name, decode_first, tabs, lines_re, blanks in JSX_TEXT_READINGS:$/        for _name, decode_first, tabs, lines_re, blanks in ():/' -- bash "$SELF" --only ts-jsx,ts-jsx2
    mc_expect_red "M250 R44: only the Babel JSX text reading is checked" "$SUBJECT_REAL" \
      's/^        for _name, decode_first, tabs, lines_re, blanks in JSX_TEXT_READINGS:$/        for _name, decode_first, tabs, lines_re, blanks in JSX_TEXT_READINGS[:1]:/' -- bash "$SELF" --only ts-jsx
    mc_expect_red "M251 R45: Babel's reading trims before it decodes" "$SUBJECT_REAL" \
      's/^            if decode_first:$/            if False:/' -- bash "$SELF" --only ts-jsx
    mc_expect_red "M252 R45: tabs inside JSX text are kept" "$SUBJECT_REAL" \
      's/^        if tabs:$/        if False:/' -- bash "$SELF" --only ts-jsx
    mc_expect_red "M253 R45: tsc does not trim NEL" "$SUBJECT_REAL" \
      's/^     "\\t\\v\\f \\u0085\\u00a0/     "\\t\\v\\f \\u00a0/' -- bash "$SELF" --only ts-jsx
    mc_expect_red "M254 R46: an uppercase JSX hex reference is decoded" "$SUBJECT_REAL" \
      's/^            if ref\[:2\] == "#x":$/            if ref[:2] in ("#x", "#X"):/' -- bash "$SELF" --only ts-jsx
    mc_expect_red "M255 R47: a lone surrogate in JSX text is not U+FFFD" "$SUBJECT_REAL" \
      's/^    return text.encode("utf-16-le", "surrogatepass").decode("utf-16-le", "replace").encode("utf-8")$/    return text.encode("utf-8", "replace")/' -- bash "$SELF" --only ts-jsx
    mc_expect_red "M256 R47: a JSX reference past U+10FFFF does not fail closed" "$SUBJECT_REAL" \
      's/^            if kind == "html_character_reference" and jsx_reference_unmodelled(text):$/            if False:/' -- bash "$SELF" --only ts-jsx,ts-jsx2
    mc_expect_red "M257 R48: a JSX text run drops the gap after a reference" "$SUBJECT_REAL" \
      's/^            runs.append(src\[start:ch.start_byte\]\.decode/            runs.append(src[start:ch.start_byte].replace(b"\\n", b"").decode/' -- bash "$SELF" --only ts-jsx,ts-jsx2
    mc_expect_red "M258 R48: a reference node with a leading blank is not range-checked" "$SUBJECT_REAL" \
      's/, ref):$/, ref if ref[:1] != b" " else b""):/' -- bash "$SELF" --only ts-jsx2
    mc_expect_red "M259 R49: a JSX text run is read without the whitespace at its edges" "$SUBJECT_REAL" \
      's/^            runs.append(src\[start:ch.start_byte\].decode("utf-8", "replace"))$/            runs.append(src[start:ch.start_byte].decode("utf-8", "replace").strip())/' -- bash "$SELF" --only ts-jsx2
    mc_expect_red "M260 R50: a JSX attribute string is not read as Babel collapses it" "$SUBJECT_REAL" \
      's/^            if value is not None and node.parent is not None and node.parent.type == "jsx_attribute":$/            if False:/' -- bash "$SELF" --only ts-jsx2
    mc_expect_red "M261 R51: JSX string readings are joined before the specifier test" "$SUBJECT_REAL" \
      's/^            for reading in readings:$/            for reading in [b"\\0".join(readings)] if readings else []:/' -- bash "$SELF" --only ts-jsx2
    mc_expect_red "M262 R53: a VT or FF in a TS/JS gap is blank" "$SUBJECT_REAL" \
      's/^LANG_BLANKS = {"bash": b" \\t\\n", \*\*{lang: b" \\t\\r\\n" for lang in JS_FAMILY}}$/LANG_BLANKS = {"bash": b" \\t\\n"}/' -- bash "$SELF" --only ts-jsx2
  }
  gated_mutants "$TS_OK" mutants_ts_rounds_33_53 ts_unmeasured "the tree-sitter arms of rounds 33-53"
  mutants_attr_system() {
    mc_expect_red "M156 R29 F1: the system attributes file is not read" "$SUBJECT_REAL" \
      's/^    if not _git_env_true("GIT_ATTR_NOSYSTEM"):$/    if False:/' -- bash "$SELF" --only paths-attrsys
    mc_expect_red "M157 R29 F1: a git that cannot name the system file reads as having none" "$SUBJECT_REAL" \
      's/^            found\.append("GIT_ATTR_SYSTEM:unresolved")$/            pass/' -- bash "$SELF" --only paths-attrsys
    mc_expect_red "M158 R29 F1: GIT_ATTR_NOSYSTEM is ignored" "$SUBJECT_REAL" \
      's/^    if v in ("", "0", "false", "no", "off"):$/    if True:/' -- bash "$SELF" --only paths-core,paths-doctest,paths-fold,paths-foldwild,paths-resolver,paths-glob24,paths-glob25,paths-glob26,paths-twin,paths-worktree,paths-wt25,paths-wt26,paths-pathspec,paths-attr,paths-attrlocal,paths-attrsys,paths-root
  }
  gated_mutants "${SYS_SHIM:-0}" mutants_attr_system unmeasured "the system attributes file: they need the POSIX git shim"
  mc_expect_red "M82 R13: an absolute target that misses this root is followed as if it landed" "$SUBJECT_REAL" \
    's/^                hits.append(link)  # absolute, and not landing on this root here$/                pass/' -- bash "$SELF" --only include-link
  mutants_ts_arms() {
    mc_expect_red "M86 R16 F1: the code_paths oracle ignores a fold twin" "$SUBJECT_REAL" \
      's/^            if inside is not False or (lang is not None and lang != c.lang):$/            if (lang is not None and lang != c.lang):/' -- bash "$SELF" --only paths-twin
    mc_expect_red "M88 R17 F1: only a whole-path fold twin counts, never an ancestor's" "$SUBJECT_REAL" \
      's/^    for i in range(len(parts), 0, -1):   # the whole path first: its reason wins$/    for i in range(len(parts), len(parts) - 1, -1):/' -- bash "$SELF" --only paths-twin
    mc_expect_red "M9 Rust doc code blocks are not compared" "$SUBJECT_REAL" \
      's/            if code_blocks(db) != code_blocks(dh):/            if False:/' -- bash "$SELF" --only rust
    mc_expect_red "M10 line breaks between TS tokens are not compared" "$SUBJECT_REAL" \
      's/        if js and last_leaf_end is not None:/        if False:/' -- bash "$SELF" --only ts-core
    mc_expect_red "M11 directive comments are dropped like prose" "$SUBJECT_REAL" \
      's/                push(("DIR", frag), node.start_byte)/                pass/' -- bash "$SELF" --only ts-core
    mc_expect_red "M12 a script reading its own source is not refused" "$SUBJECT_REAL" \
      's/^        if shell_self_reads(tree, work, own):$/        if False:/' -- bash "$SELF" --only shell-core,shell-r33,shell-r35
    mc_expect_red "M13 string-like nodes are no longer compared whole" "$SUBJECT_REAL" \
      's/        if node.child_count == 0 or node.type in atomic:/        if node.child_count == 0:/' -- bash "$SELF" --only ts-core
    mc_expect_red "M14 the working tree is read off disk instead of through git" "$SUBJECT_REAL" \
      's/                c.head = reconstruct_worktree(root, base_sha, c.path, c.base or b"")/                c.head = open(os.path.join(root, *c.path.split("\/")), "rb").read()/' -- bash "$SELF" --only shell-core
    mc_expect_red "M16 a self-read through a variable is not refused" "$SUBJECT_REAL" \
      's/^        if shell_may_self_read(tree, work, own):$/        if False:/' -- bash "$SELF" --only shell-core,shell-r33
    mc_expect_red "M19 a reader under a wrapper command is not a reader" "$SUBJECT_REAL" \
      's/    if base in SHELL_WRAPPERS:/    if False:/' -- bash "$SELF" --only shell-core,shell-r33
    mc_expect_red "M20 a numbered redirect's operator is looked for only in its first child" "$SUBJECT_REAL" \
      's/    return node.type == "file_redirect" and any(ch.type in SHELL_READ_REDIRECTS for ch in node.children)/    return node.type == "file_redirect" and node.child_count > 0 and node.children[0].type in SHELL_READ_REDIRECTS/' -- bash "$SELF" --only shell-core
    # Unmerged, c21's edit lands in its own group, which the whitelist refuses
    # for its fence line, so the case's code-block reason reddens.
    mc_expect_red "M25 doc fragments split by an attribute are not merged" "$SUBJECT_REAL" \
      's/doc_continues(tokens, last_doc\[1\] + 1, DOC_ATTR_NODES\[doc\])/last_doc[1] == len(tokens) - 1/' -- bash "$SELF" --only rust
    mc_expect_red "M27 A.iii: a bare CR in a doc line is not refused" "$SUBJECT_REAL" \
      's/^    if "\\r" in line:$/    if False:/' -- bash "$SELF" --only rust
    mc_expect_red "M31 a doc group joined with a doc attribute is not refused" "$SUBJECT_REAL" \
      's/^            return "rust_doc_joined_attribute"$/            pass/' -- bash "$SELF" --only rust
    mc_expect_red "M32 A.v: inner docs inside a block are not refused" "$SUBJECT_REAL" \
      's/^        return None if parent.type == "source_file" else "rust_doc_inner_in_block"$/        return None/' -- bash "$SELF" --only rust
    mc_expect_red "M33 a module file's inner docs are not looked up against the parent's" "$SUBJECT_REAL" \
      's/^            why = parent_module_docs(rust_trees\[side\], path)$/            why = None/' -- bash "$SELF" --only rust
    mc_expect_red "M34 A.i: fragments with something between them read as one consecutive group" "$SUBJECT_REAL" \
      's/^            return "rust_doc_not_contiguous"$/            pass/' -- bash "$SELF" --only rust
    mc_expect_red "M35 A.ii: raw doc lines are not checked for indentation or block starts" "$SUBJECT_REAL" \
      's/^        why = _doc_line_refusal(_doc_raw_line(n, src))$/        why = None/' -- bash "$SELF" --only rust
    mc_expect_red "M36 A.iv: a doc inside a macro or on a mod item is not refused" "$SUBJECT_REAL" \
      's/^    why = rust_doc_item_refusal(nodes\[0\], item)$/    why = None/' -- bash "$SELF" --only rust
    mc_expect_red "M37 a /** */ doc fragment is not refused" "$SUBJECT_REAL" \
      's/^        return "rust_doc_block_comment"$/        pass/' -- bash "$SELF" --only rust
    mc_expect_red "M40 A.vi: a doc under a doc-reading attribute is not refused" "$SUBJECT_REAL" \
      's/^            return "rust_doc_attribute_reader:" + name$/            pass/' -- bash "$SELF" --only rust
    mc_expect_red "M41 A.vii: an outer doc joined with inner docs in its item's body is not refused" "$SUBJECT_REAL" \
      's/^    if _subtree_has_doc_fragment(item, src):$/    if False:/' -- bash "$SELF" --only rust
    # c38's form feed (\x0c) is the case this narrowed range lets through.
    mc_expect_red "M47 A.iii: _DOC_CONTROL_RE no longer covers LF..FF" "$SUBJECT_REAL" \
      '/^_DOC_CONTROL_RE = /s/x0a-/x0d-/' -- bash "$SELF" --only rust
    mc_expect_red "M48 an unreadable parent module reads non_executable instead of unknown" "$SUBJECT_REAL" \
      's/^            return UNKNOWN, "rust_doc_parent_module_unread"$/            return NON_EXECUTABLE, ""/' -- bash "$SELF" --only rust
  }
  gated_mutants "$TS_OK" mutants_ts_arms ts_unmeasured "the tree-sitter arms, and paths-twin's language twin"
fi

EXECUTED=$((PASS + FAIL))
echo "exec-equivalence-test: executed=$EXECUTED passed=$PASS failed=$FAIL unmeasured=$UNMEASURED"
mc_graded && mc_trailer "$EXECUTED"
if [ "$EXECUTED" -eq 0 ]; then
  echo "FAIL: zero cases executed -- a run that asserted nothing is not a pass"
  exit 1
fi
[ "$FAIL" -eq 0 ]
