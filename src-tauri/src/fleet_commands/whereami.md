---
description: "Print this session's context card in two separated blocks: IDENTITY (fixed for the session's lifetime) and REACHABILITY (true only right now). Answers \"am I inside the runner?\" from $QONTINUI_RUNNER_CONTEXT, never from a port probe."
allowed-tools: Read, Glob, Grep, Bash, PowerShell
---

# /whereami — session context card

Print **two clearly separated blocks**. They answer different questions and are
true for different lengths of time, so they never share a line:

| Block | Question | Lifetime |
|---|---|---|
| **IDENTITY (fixed)** | Who spawned me, as what, where? | Fixed for this session |
| **REACHABILITY (now)** | What answers me at this instant? | True only at probe time |

**The identity predicate is `$QONTINUI_RUNNER_CONTEXT` being non-empty — iff.**
The runner injects it at spawn
(`qontinui-runner/src-tauri/src/terminal/session.rs:753`) from the single source
of truth `terminal::runner_context()` (`terminal/mod.rs:338`), and its first line
is the attributable marker
`[source: qontinui-runner/runner_context@<version>+<git-sha>]`
(`RUNNER_CONTEXT_SOURCE_MARKER`, `terminal/mod.rs:211`). It is fixed for the
session's lifetime and survives a runner restart.

**Line 2, where present, is the PROVENANCE line** — a sequence of
`[key: value]` tokens, not a single token. The first is the briefing
provenance: which briefing text this session is actually running under, one of
`[briefing: coord session_briefing/runner-session v<N>]`,
`[briefing: cached v<N> (stale)]`, `[briefing: builtin-fallback]` or
`[briefing: builtin-fallback (rejected coord v<N>)]` (coord served a body that
failed the runner's render-time invariant guard). The briefing
body is a coord `session_briefing` prompt document, with the compiled-in Rust
constant demoted to a labelled fallback (plan
`2026-08-20-runner-session-briefing-versioned-and-operator-editable`; mechanism in
`knowledge-base/qontinui-specific/runner-development.md`). A **second token**,
` [clause: <provenance>]`, follows it whenever the fleet plan-capture dial reads
`record` — so the line reads `[briefing: builtin-fallback] [clause:
builtin-fallback]`, a shape the runner pins byte-for-byte in its own test. Cut
each token at its own first `]`; parsing to the end of the line swallows the
clause into the briefing row. Line 1 is unchanged and
byte-identical either way — the marker contract is line-1 **equality**, so the
provenance had to go on its own line. A runner built before that plan emits no
line 2 at all: report that as `<none>`, never as `builtin-fallback`. And when
the whole variable is unset the row is `<n/a>`, not `<none>` — with no context
there is no briefing to have provenance for, and `<none>` would assert a runner
build this card never saw.

Three things this card exists to stop you concluding:

1. **A `:9876` probe is not an identity test.** It answers "is the runner API up
   right now". It goes false on every restart and false on a wedged runner,
   while identity does not move.
2. **The context is SPAWNER identity, not live-binary identity.** After a
   rebuild + restart it still names the build that spawned you. Step 4 below
   cross-checks it against the live `buildId` and reports disagreement as a
   finding, not an error — a session cannot read its own appended system prompt,
   so this comparison is the only way to see the stale-binary condition.
3. **The headless seam EXPORTS the variable but need not pass the briefing to
   the model.** An earlier note here said `agent_runtime::spawn_claude_child`
   set neither; that is stale — `finalize_headless_child_env`
   (`qontinui-runner/src-tauri/src/agent_runtime.rs:4371`, called in production
   at `:4442` and exercised by a test at `:4766`) exports
   `QONTINUI_RUNNER_CONTEXT` rendered from the same
   `terminal::runner_context()`. What it deliberately does NOT do is add
   `--append-system-prompt` (its own doc says so), so on that path the predicate
   is answerable while the briefing may never reach the model. Still report an
   unset variable in a headless session as UNKNOWN rather than "not inside": the
   guarantee is per call site, not global.

### Why not the neighbouring variables

- **`QONTINUI_RUNNER_ID` names WHICH runner, not WHETHER you are inside one.**
  It is live and stable in a real session (`primary` on this box), but the
  supervisor sets it on the runner process
  (`qontinui-supervisor/src/process/env_forwarders.rs:810`) and the session
  inherits it — so it carries no attributable build marker and answers a
  different question. Step 1 prints it as context, and it is never the
  predicate. (An earlier note here claimed unit tests could poison it with
  `"test-runner-42"` / `"unit-test"`; both `set_var` calls are inside
  `#[cfg(test)] mod tests` (`startup_panic.rs:201-202`) behind an `EnvGuard`
  that removes them on drop, and a `set_var` in a `cargo test` process mutates
  that process only. That hazard cannot reach a session.)
- **`$QONTINUI_PLANS_DIR`** is an operator-exportable path — settable anywhere,
  so it proves nothing.

## Rules that make this card safe to run anywhere

- **Allow-list reads only.** Name every variable you read. A whole-environment
  dump is what leaks: the session environment carries three PLAINTEXT passwords
  (`QONTINUI_OPERATOR2_PASSWORD`, `QONTINUI_TEST_LOGIN_PASSWORD`,
  `QONTINUI_TEST_AUTO_LOGIN_PASSWORD`), and the habitual redaction filter over
  `JWT`/`KEY`/`TOKEN`/`SECRET` matches none of them. An allow-list is safe by
  construction; a deny-list is one new variable away from a leak.
- **Never print key material.** The proxy nonces found in Step 3 are secrets:
  print a truncated digest so distinct nonces stay distinguishable, never the
  nonce.
- **Never put a nonce on a command line.** Process cmdlines are world-readable on
  this multi-session machine. Stage the header in a private tempfile, pass it
  with curl's header-from-file form, and delete it on exit.
- **`http://127.0.0.1:<port>`, never `localhost`.** The runner binds the IPv4
  loopback only while Windows resolves the name to `::1` first, so the name pays
  a doomed IPv6 connect first (lint check #14).
- **Distinguish DOWN from UNKNOWN on every probe.** A refused connection proves
  nothing is listening. A timeout proves nothing at all — on a loaded box
  `/health` has been sampled from 296 ms to 10120 ms. Report `UNKNOWN`, never
  "absent", for anything that is not a refusal.

## Step 1 — IDENTITY (allow-list reads only)

```bash
# Named reads only. Never dump the environment - see the rules above.
CTX="$(printenv QONTINUI_RUNNER_CONTEXT 2>/dev/null)"
RUNNER_ID="$(printenv QONTINUI_RUNNER_ID 2>/dev/null)"
API_PORT="$(printenv QONTINUI_RUNNER_API_PORT 2>/dev/null)"
TERMINAL_ID="$(printenv QONTINUI_TERMINAL_ID 2>/dev/null)"
TIER="$(printenv QONTINUI_AGENT_TIER 2>/dev/null)"
WT_MODE="$(printenv QONTINUI_AGENT_WORKTREE_MODE 2>/dev/null)"
PLANS_DIR="$(printenv QONTINUI_PLANS_DIR 2>/dev/null)"

# The context's FIRST line is the attributable source marker; its SECOND, on a
# runner that has one, is the provenance line. The briefing body itself is not
# printed. Parse with parameter expansion - no awk field references, which the
# harness would rewrite (lint check #18).
SPAWN_VER=""; SPAWN_SHA=""; BRIEFING=""; CLAUSE=""
if [ -n "$CTX" ]; then
  MARKER="$(printf '%s\n' "$CTX" | head -n 1)"
  case "$MARKER" in
    *"runner_context@"*)
      REST="${MARKER#*runner_context@}"
      # Cut every field at the FIRST `]`, never at the end of the line. A
      # `${VAR%]}` that strips one TRAILING bracket silently keeps whatever
      # follows it, and `${VAR%%+*}` on a marker with no `+` returns the string
      # UNCHANGED - which printed the version as `1.0.8]`, bracket included.
      SPAWN_VER="${REST%%+*}"; SPAWN_VER="${SPAWN_VER%%]*}"
      SPAWN_SHA="${REST#*+}";  SPAWN_SHA="${SPAWN_SHA%%]*}"
      ;;
  esac
  # Hex shape-guard. `${VAR#pattern}` returns the string UNCHANGED on no match,
  # so a marker of an unexpected shape would otherwise be printed verbatim as
  # though it were a sha. `unknown` (a source-tarball build with no git) is
  # non-hex, so this rejects that too. SPAWN_VER is deliberately NOT guarded: it
  # is display-only and never compared, and a version can carry a pre-release
  # suffix that no cheap shape test should blank.
  case "$SPAWN_SHA" in
    *[!0-9a-f]* | '') SPAWN_SHA="" ;;
  esac
  # LINE 2 IS A SEQUENCE OF `[key: value]` TOKENS, NOT ONE TOKEN. Whenever the
  # fleet plan-capture dial reads `record`, the runner appends a second token -
  # `[briefing: <base>] [clause: <clause>]` (qontinui-runner
  # `terminal/mod.rs:383`, pinned byte-for-byte by its own test at `:1224`).
  # Parsing to the END of the line therefore swallowed the clause into the
  # briefing row and left it carrying an unbalanced `]`. Cut each token at its
  # own first `]`; a token that is absent stays empty.
  LINE2="$(printf '%s\n' "$CTX" | sed -n '2p')"
  case "$LINE2" in
    "[briefing: "*) BRIEFING="${LINE2#\[briefing: }"; BRIEFING="${BRIEFING%%]*}" ;;
  esac
  case "$LINE2" in
    *"[clause: "*) CLAUSE="${LINE2#*\[clause: }"; CLAUSE="${CLAUSE%%]*}" ;;
  esac
fi

# Three states for the briefing row, not two. With NO context at all there is no
# briefing to have provenance for, so blaming an old runner build for the
# missing line asserts a cause this card never established - the same
# fabrication class as reporting a timeout as an absence. `<none>` is a claim
# ABOUT a runner build; make it only when a runner spoke.
if [ -z "$CTX" ]; then BRIEFING_ROW="<n/a - no runner context>"
elif [ -n "$BRIEFING" ]; then BRIEFING_ROW="$BRIEFING"
else BRIEFING_ROW="<none - runner predates briefing provenance>"; fi

# The clause row distinguishes THREE absences the briefing row cannot. A
# briefing token with no clause token beside it is a runner that DOES emit
# provenance and simply has the dial off - a normal state, not a missing
# feature - so it is reported as such rather than as `<none>`.
if [ -z "$CTX" ]; then CLAUSE_ROW="<n/a - no runner context>"
elif [ -n "$CLAUSE" ]; then CLAUSE_ROW="$CLAUSE"
elif [ -n "$BRIEFING" ]; then CLAUSE_ROW="<absent - plan-capture dial is off>"
else CLAUSE_ROW="<n/a - runner predates briefing provenance>"; fi

if [ -n "$CTX" ]; then INSIDE="YES"; else INSIDE="NO (or a headless spawn - see note 3)"; fi
printf 'inside runner : %s\n' "$INSIDE"
printf 'runner id     : %s\n' "${RUNNER_ID:-<unset>}"
printf 'context       : version %s sha %s\n' "${SPAWN_VER:-<unparsed>}" "${SPAWN_SHA:-<unparsed>}"
printf 'briefing      : %s\n' "$BRIEFING_ROW"
printf 'clause        : %s\n' "$CLAUSE_ROW"
printf 'tier          : %s\n' "${TIER:-<unset>}"
printf 'terminal id   : %s\n' "${TERMINAL_ID:-<unset>}"
printf 'worktree mode : %s\n' "${WT_MODE:-<unset>}"
printf 'plans dir     : %s\n' "${PLANS_DIR:-<unset - optional, plans may live only in the corpus>}"
printf 'cwd           : %s\n' "$PWD"
```

An unset `QONTINUI_AGENT_TIER` or `QONTINUI_AGENT_WORKTREE_MODE` is normal on an
interactive pane; report it as `<unset>`, not as a tier of zero.

## Step 2 — REACHABILITY (now)

**Every block in this file re-derives what it needs.** Each fenced block is a
separate Bash tool invocation and **Bash tool shell state does not persist
between calls** — a variable set in one block is EMPTY in the next (verified
2026-08-18: `FOO=hello` in call 1 read back as `FOO=[]` in call 2). Inheriting
`$API_PORT` from Step 1 would probe `9876` on a secondary instance that
announced `9877` and report a false `DOWN (connection refused)`, on the one card
whose whole purpose is not to conflate reachability with absence. The re-reads
are `printenv`, so they cost nothing; do NOT replace them with a note telling
the reader to run the blocks as one call.

```bash
# Re-derived, not inherited - see above.
API_PORT="$(printenv QONTINUI_RUNNER_API_PORT 2>/dev/null)"

# Probe result classes, all three distinct:
#   answered  - the HTTP code it returned
#   DOWN      - curl exit 7, connection refused: nothing is listening
#   UNKNOWN   - anything else (timeout, reset, resolution): proves nothing
probe() {
  PROBE_OUT="$(curl -s --connect-timeout 3 -m 15 -o /dev/null -w '%{http_code}' "$PROBE_URL" 2>/dev/null)"
  PROBE_RC=$?
  if [ "$PROBE_RC" = "0" ]; then PROBE_VERDICT="up (HTTP $PROBE_OUT)"
  elif [ "$PROBE_RC" = "7" ]; then PROBE_VERDICT="DOWN (connection refused)"
  else PROBE_VERDICT="UNKNOWN (curl exit $PROBE_RC - not evidence of absence)"; fi
}

RPORT="${API_PORT:-9876}"
PROBE_URL="http://127.0.0.1:$RPORT/health"; probe
printf 'runner  :%s  %s\n' "$RPORT" "$PROBE_VERDICT"
RUNNER_VERDICT="$PROBE_VERDICT"

PROBE_URL="http://127.0.0.1:9875/health"; probe
printf 'supervisor :9875  %s\n' "$PROBE_VERDICT"
```

## Step 3 — which `.mcp.json` holds a LIVE coord proxy

**Compare `(port, nonce)` pairs, not nonces alone.** Measured on the operator box
2026-08-18: 13 `.mcp.json` files carried 13 DISTINCT nonces across TWO ports — 10
targeting the primary instance's `/coord-mcp` and 3 targeting a secondary
instance on another port. A file is therefore "dead" for two unrelated reasons
that must not be conflated: its nonce was evicted (the instance is up and says
401), or its whole instance is not running (nothing is listening on its port).
Probe each candidate against **its own url** with **its own nonce**.

```bash
# Workspace root: $QONTINUI_ROOT wins; else the parent of the MAIN checkout via
# --git-common-dir (NOT --show-toplevel, which inside a linked worktree names the
# worktree container and makes this sweep probe nothing); else $PWD.
ROOT="${QONTINUI_ROOT:-}"
if [ -z "$ROOT" ]; then
  GC="$(git rev-parse --git-common-dir 2>/dev/null)"
  [ -n "$GC" ] && GC="$(cd "$GC" 2>/dev/null && pwd)"
  [ -n "$GC" ] && ROOT="$(dirname "$(dirname "$GC")")"
fi
[ -z "$ROOT" ] || [ "$ROOT" = "." ] && ROOT="$PWD"

# BOUND THE SWEEP. Measured 2026-08-18: the unbounded form, with the worktree
# glob expanded, was still probing after five minutes on this box - every dead
# candidate on a portless instance costs a full connect timeout. Take the repo
# checkouts first (where a live nonce actually lives), then worktrees, and stop
# at MAX_CANDIDATES. A truncated sweep is reported as truncated, never as
# "no live proxy".
MAX_CANDIDATES=12
CANDIDATES=("$PWD/.mcp.json" "$ROOT/.mcp.json")
while IFS= read -r f; do
  [ "${#CANDIDATES[@]}" -ge "$MAX_CANDIDATES" ] && break
  for c in "${CANDIDATES[@]}"; do [ "$c" = "$f" ] && continue 2; done
  CANDIDATES+=("$f")
done < <(ls "$ROOT"/*/.mcp.json "$ROOT"/_wt*/*/.mcp.json 2>/dev/null)

# The nonce is key material: stage it in a private tempfile and hand it to curl
# from that file. cygpath because a native curl.exe cannot open an MSYS path
# under an inherited MSYS_NO_PATHCONV.
HDR="$(mktemp)" || { echo "mktemp failed - cannot stage the nonce off argv" >&2; exit 1; }
trap 'rm -f "$HDR"' EXIT
hdrp() { command -v cygpath >/dev/null 2>&1 && cygpath -w "$HDR" || printf '%s' "$HDR"; }

# Pick a JSON reader up front. BOTH readers resolve on this box (measured
# 2026-08-18: `command -v jq` -> a scoop shim under the <windows-user> profile;
# `command -v python` -> the Python313 install), so the dual arm is portability
# insurance for a box without one, NOT a workaround for a missing jq. Fail LOUD
# if neither exists - a missing tool must never read as "no live proxy".
#
# Both readers take the config on STDIN or as a converted Windows path. Native
# python.exe cannot open an MSYS `/<drive>/...` path, and under an inherited
# MSYS_NO_PATHCONV / MSYS2_ARG_CONV_EXCL the automatic argv conversion is OFF
# (verified 2026-08-18: MSYS_NO_PATHCONV=1 -> FileNotFoundError on the MSYS
# spelling of <workspace-root>/.../.mcp.json; the same call with `cygpath -w`
# returned the url).
# `2>/dev/null` then swallows the traceback, the url comes back empty, and the
# candidate is silently skipped - a fabricated negative. So convert, exactly as
# the curl header file is converted above.
cfgp() { command -v cygpath >/dev/null 2>&1 && cygpath -w "$MCP_CFG" || printf '%s' "$MCP_CFG"; }
SWEEP_UNKNOWN=0
if command -v jq >/dev/null 2>&1; then
  mcp_url() { jq -r '.mcpServers["coord-mcp"].url // ""' < "$MCP_CFG" 2>/dev/null; }
  # BOTH header shapes. Plan 2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning
  # Phase 2 moves the proxy nonce out of the custom `X-Coord-Mcp-Proxy-Key`
  # header and into `Authorization: Bearer <nonce>` -- a custom header makes the
  # MCP client attach an OAuth provider, so a stale-key 401 escalates into
  # discovery and then Dynamic Client Registration, which the runner 404s. The
  # runner keeps accepting the legacy header, so BOTH shapes sit on disk
  # indefinitely (configs are rewritten only on session spawn). Reading only the
  # legacy name would empty `key` on exactly the configs the fix produces and
  # this sweep would report "no live proxy" over a workspace full of live doors.
  # `Authorization` wins when both are present, mirroring the runner's own
  # precedence; the value is kept VERBATIM (`Bearer ` prefix included), and
  # `mcp_keyhdr` reports which header name to stage it under.
  mcp_key() { jq -r '(.mcpServers["coord-mcp"].headers // {}) as $h | if (($h.Authorization // "") | tostring) != "" then $h.Authorization else ($h["X-Coord-Mcp-Proxy-Key"] // "") end' < "$MCP_CFG" 2>/dev/null; }
  mcp_keyhdr() { jq -r 'if (((.mcpServers["coord-mcp"].headers.Authorization // "") | tostring) != "") then "Authorization" else "X-Coord-Mcp-Proxy-Key" end' < "$MCP_CFG" 2>/dev/null; }
elif command -v python >/dev/null 2>&1; then
  mcp_url() { python -c "import json,sys;print(json.load(open(sys.argv[1],encoding='utf-8')).get('mcpServers',{}).get('coord-mcp',{}).get('url',''))" "$(cfgp)" 2>/dev/null; }
  mcp_key() { python -c "import json,sys;h=json.load(open(sys.argv[1],encoding='utf-8')).get('mcpServers',{}).get('coord-mcp',{}).get('headers',{});print(h.get('Authorization') or h.get('X-Coord-Mcp-Proxy-Key','') or '')" "$(cfgp)" 2>/dev/null; }
  mcp_keyhdr() { python -c "import json,sys;h=json.load(open(sys.argv[1],encoding='utf-8')).get('mcpServers',{}).get('coord-mcp',{}).get('headers',{});print('Authorization' if h.get('Authorization') else 'X-Coord-Mcp-Proxy-Key')" "$(cfgp)" 2>/dev/null; }
else
  # NOT a fall-through. Without a reader every candidate yields an empty url,
  # every url fails the /coord-mcp match, and the summary below would report
  # "no live proxy among the N files swept" - a coord verdict manufactured out
  # of a missing tool. Stop instead, and mark the sweep UNKNOWN for any caller
  # that catches the exit.
  SWEEP_UNKNOWN=1
  echo "neither jq nor python can read .mcp.json - the sweep is UNKNOWN, not empty" >&2
  exit 1
fi

RPC='{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
for f in "${CANDIDATES[@]}"; do
  [ -r "$f" ] || continue
  MCP_CFG="$f"
  url="$(mcp_url)"; key="$(mcp_key)"
  case "$url" in *"/coord-mcp"*) ;; *) continue ;; esac
  [ -n "$key" ] || { printf '%-70s no nonce\n' "$f"; continue; }
  # Fingerprint, never the nonce itself.
  fp="$(printf '%s' "$key" | sha256sum 2>/dev/null | cut -c1-8)"
  { printf '%s: %s\n' "$(mcp_keyhdr)" "$key" > "$HDR"; } 2>/dev/null
  [ -s "$HDR" ] || { echo "cannot stage the nonce header - LOCAL fault, not a verdict" >&2; break; }
  code="$(curl -s --connect-timeout 2 -m 10 -o /dev/null -w '%{http_code}' -X POST "$url" \
    -H "Content-Type: application/json" -H @"$(hdrp)" -d "$RPC" 2>/dev/null)"
  rc=$?
  # Same three classes as Step 2, and the same trap: curl exit 7 is a REFUSAL
  # (nothing listening); exit 28 is a TIMEOUT and proves nothing. Measured
  # 2026-08-18: with a 10s budget this loop returned exit 28 for candidates on
  # the PRIMARY port that had answered 401 moments earlier, so a timeout
  # rendered as "no listener" would have invented a dead instance. Budget
  # against the tail, and report the tail honestly.
  if [ "$rc" = "7" ]; then verdict="no listener on that port (refused)"
  elif [ "$rc" != "0" ]; then verdict="UNKNOWN (curl exit $rc - not evidence of absence)"
  elif [ "$code" = "200" ]; then verdict="LIVE"
  elif [ "$code" = "401" ]; then verdict="nonce evicted (401)"
  else verdict="answered HTTP $code"; fi
  printf '%-70s %-40s nonce#%s  %s\n' "$f" "$url" "${fp:-?}" "$verdict"
done
rm -f "$HDR"
```

Report the **live** file explicitly — that file decides which tenant a coord read
acts as. A file that is dead because its port has no listener is not evidence
about the primary instance, and a sweep that hit `MAX_CANDIDATES` is TRUNCATED:
say so instead of concluding no door is live. If nothing came back LIVE, the
honest line is "no live proxy among the N files swept" — this session's own
nonce may live in a per-session config the sweep never visited, and `/coord-revive`
is the tool that runs the full cascade.

If the block exited non-zero on `SWEEP_UNKNOWN` (no JSON reader), the proxy row
is `UNKNOWN (no JSON reader)` — **never** "no live proxy". A missing tool is a
local fault, not a verdict about coord.

Measured on this box 2026-08-18 (bounded sweep, 13 files / 13 distinct nonces /
2 ports): the `$ROOT/*/.mcp.json` files targeting the primary port answered
**401** — the instance is up and evicted their nonce — while every
`_wt*/qontinui-schemas/.mcp.json` targeting the secondary instance's port failed
with **curl exit 7**, no listener at all. Two different deaths that only the
`(port, nonce)` pairing tells apart. A second pass under load returned **exit
28** for several of the same primary-port files: same files, same instance, and
a verdict that would have flipped from "evicted" to "instance gone" purely
because the box was busy. That is why the timeout class is reported as UNKNOWN.

This sweep is the slow step: measured 2026-08-18 at roughly eight minutes for 11
candidates on a loaded box, because each dead candidate costs its whole budget.
Steps 1, 2 and 4 together take seconds. When you only need the identity half, run
those three and print the reachability block with the proxy row marked
`not swept` — an unswept row is not a dead one.

## Step 4 — spawn-time build vs live build

```bash
# Re-derived, NOT inherited from Step 1 - shell state does not survive between
# Bash tool calls (see Step 2). An empty $SPAWN_SHA here would print UNKNOWN on
# every run, including the stale-binary case this cross-check exists to catch.
CTX="$(printenv QONTINUI_RUNNER_CONTEXT 2>/dev/null)"
API_PORT="$(printenv QONTINUI_RUNNER_API_PORT 2>/dev/null)"
SPAWN_SHA=""
if [ -n "$CTX" ]; then
  MARKER="$(printf '%s\n' "$CTX" | head -n 1)"
  case "$MARKER" in
    *"runner_context@"*)
      REST="${MARKER#*runner_context@}"
      SPAWN_SHA="${REST#*+}"
      SPAWN_SHA="${SPAWN_SHA%%]*}"
      ;;
  esac
  # SHAPE-GUARD the spawn side too - same reason as the live side below.
  # `${VAR#pattern}` returns the string UNCHANGED when the pattern does not
  # match, so a marker whose shape drifts parses to something non-empty that is
  # not a sha, sails past the -z UNKNOWN test, and the comparison then
  # manufactures a confident AGREE or DISAGREE out of it. A sha is lowercase hex.
  case "$SPAWN_SHA" in
    *[!0-9a-f]* | '') SPAWN_SHA="" ;;
  esac
fi

# THE LIVE SHA IS `data.gitSha`, NOT THE TOP-LEVEL `buildId`.
#
# `gitSha` is `env!("QONTINUI_GIT_SHA")` - the same `git rev-parse --short=12
# HEAD` stamp `build.rs` bakes into the marker - so the comparison below is
# EXACT equality, not a prefix test.
#
# `buildId` is a different sha from a different build step: the embedded Vite
# dist's id, `<9-char-sha>-<unix-ms>` written by `vite.config.ts`, or the
# `unstamped-<sha>` sentinel when the exe was built with no `dist/`. The
# runner's own source says so at the field - "NOT a staleness signal ... For
# 'is this runner out of date', use `buildDrift`" (`mcp_api.rs`, the `buildId`
# arm). Comparing it against the marker mis-fires in BOTH directions on the
# ordinary inner dev loop, where a bare `cargo build` moves the binary but not
# the dist: a Rust-only rebuild gives DISAGREE about the very binary that
# spawned you, and an older session sees the unchanged dist id and reads AGREE
# straight through a real binary swap. This card previously read `buildId`; the
# bidirectional prefix comparison it needed existed only to paper over the
# 9-versus-12-character mismatch between two unrelated shas.
LIVE_BUILD="$(curl -s --connect-timeout 3 -m 20 "http://127.0.0.1:${API_PORT:-9876}/health" 2>/dev/null)"
LIVE_SHA=""
case "$LIVE_BUILD" in
  *'"gitSha":"'*)
    LIVE_SHA="${LIVE_BUILD#*\"gitSha\":\"}"
    LIVE_SHA="${LIVE_SHA%%\"*}"
    ;;
esac

# SHAPE-GUARD the parse. `${VAR#pattern}` returns the string UNCHANGED when the
# pattern does not match, so `{"gitSha":null}` parses to the literal `{` -
# non-empty, so it would sail past the -z UNKNOWN test below and the card would
# assert DISAGREE ("rebuilt and restarted after this session started") on no
# evidence. A sha is lowercase hex; anything else is UNKNOWN.
case "$LIVE_SHA" in
  *[!0-9a-f]* | '') LIVE_SHA="" ;;
esac
# `unknown` is the literal a source-tarball build with no git emits for the sha
# component (qontinui-runner `terminal/mod.rs:208`, the doc on
# `RUNNER_CONTEXT_SOURCE_MARKER` at `:211`) - so it can appear on BOTH sides at
# once, and an equality test would then render two UNKNOWNs as a confident
# AGREE. Both sides carry the hex guard, which already rejects it; the by-name
# rejection stays as a readable assertion of that intent.
[ "$LIVE_SHA" = "unknown" ] && LIVE_SHA=""
[ "$SPAWN_SHA" = "unknown" ] && SPAWN_SHA=""

if [ -z "$SPAWN_SHA" ] || [ -z "$LIVE_SHA" ]; then
  printf 'build cross-check  UNKNOWN (spawn=%s live=%s)\n' "${SPAWN_SHA:-?}" "${LIVE_SHA:-?}"
elif [ "$SPAWN_SHA" = "$LIVE_SHA" ]; then
  printf 'build cross-check  AGREE (spawn %s = live %s)\n' "$SPAWN_SHA" "$LIVE_SHA"
else
  printf 'build cross-check  DISAGREE - spawned by %s, talking to %s\n' "$SPAWN_SHA" "$LIVE_SHA"
fi
```

Exact equality, because both sides are now the same 12-character
`git rev-parse --short=12 HEAD` stamp. DISAGREE is a **finding, not an error**:
the runner was rebuilt and restarted after this session started, so anything you
conclude from the context marker describes the old binary. Say so in the card
rather than silently preferring one.

`buildDrift` in the same `/health` body answers a **different** question — how
far the running build is behind `origin/main`. A runner can be many commits
behind and still AGREE here, because AGREE means "the binary that spawned me is
the binary I am talking to", not "the binary is current".

## Fallback when bash hangs

msys `bash` has been observed hanging on this box where PowerShell works — switch
rather than retrying. This one block carries the **whole** card: Step 1's IDENTITY
rows, Step 2's port probes and Step 4's build cross-check.

**What it does NOT carry is Step 3's proxy sweep** — that sweep needs a JSON
reader, a private header file and a per-candidate POST, and there is no
PowerShell twin of it here. Print the proxy row as `not swept`, exactly as Step 3
itself instructs when you skip it. An unswept row is not a dead one, and a
fallback that silently drops a contract row reads as "there is no live proxy".

```powershell
# `Invoke-WebRequest` renders a progress bar on 5.1 that costs real time and
# pollutes captured output - in a block whose whole premise is "bash hung".
$ProgressPreference = 'SilentlyContinue'

$names = 'QONTINUI_RUNNER_CONTEXT','QONTINUI_RUNNER_ID','QONTINUI_RUNNER_API_PORT',
         'QONTINUI_TERMINAL_ID','QONTINUI_AGENT_TIER','QONTINUI_AGENT_WORKTREE_MODE','QONTINUI_PLANS_DIR'
$vals = @{}
foreach ($n in $names) { $vals[$n] = [Environment]::GetEnvironmentVariable($n) }
$spawnVer = ''; $spawnSha = ''; $briefing = ''; $clause = ''
if ($vals['QONTINUI_RUNNER_CONTEXT']) {
  $ctxLines = $vals['QONTINUI_RUNNER_CONTEXT'] -split "`n"
  $marker = $ctxLines[0]
  # A regex, not a string strip, and the case-SENSITIVE operators throughout.
  # Bash's `${VAR#pattern}` returns the string UNCHANGED when the pattern
  # misses, which is why Step 1 and Step 4 each need an explicit hex guard
  # AFTER the parse; a regex that misses simply does not match, so here the
  # guard lives in the pattern. The `c` is load-bearing: `-match`, `-like` and
  # `-replace` are all case-INSENSITIVE by default, so `[0-9a-f]` would accept
  # an uppercase sha that the bash guard rejects and the two renders would
  # disagree about the same marker.
  #
  # `(\]|$)` pins the hex run to the token boundary, matching bash's cut at the
  # first `]`: without it, `+218a39e18c26junk]` would parse to a confident
  # `218a39e18c26` here while bash blanks it. The version is matched separately
  # and does NOT require a `+`, because bash's `${REST%%+*}` still yields a
  # version on a marker that has none - it is display-only and never compared.
  if ($marker -cmatch 'runner_context@([^+\]]+)')                { $spawnVer = $Matches[1] }
  if ($marker -cmatch 'runner_context@[^+]+\+([0-9a-f]+)(\]|$)') { $spawnSha = $Matches[1] }
  # LINE 2 IS A SEQUENCE OF `[key: value]` TOKENS, NOT ONE TOKEN - the runner
  # appends ` [clause: <clause>]` whenever the fleet plan-capture dial reads
  # `record`. `[^\]]*` cuts each token at its own first `]`, exactly as the bash
  # twin's `%%]*` does.
  if ($ctxLines.Count -ge 2) {
    if ($ctxLines[1] -cmatch '\[briefing: ([^\]]*)\]') { $briefing = $Matches[1] }
    if ($ctxLines[1] -cmatch '\[clause: ([^\]]*)\]')   { $clause   = $Matches[1] }
  }
}
# Same three states as Step 1, and for the same reason: `<none>` asserts
# something about a runner BUILD, so it is only sayable when a runner spoke.
if (-not $vals['QONTINUI_RUNNER_CONTEXT']) { $briefingRow = '<n/a - no runner context>' }
elseif ($briefing)                         { $briefingRow = $briefing }
else                                       { $briefingRow = '<none - runner predates briefing provenance>' }

if (-not $vals['QONTINUI_RUNNER_CONTEXT']) { $clauseRow = '<n/a - no runner context>' }
elseif ($clause)                           { $clauseRow = $clause }
elseif ($briefing)                         { $clauseRow = '<absent - plan-capture dial is off>' }
else                                       { $clauseRow = '<n/a - runner predates briefing provenance>' }

# The prose above promises BOTH halves, so print both - and keep them under
# their own headers even here, where one block spans the two. A single
# undifferentiated list is exactly the conflation this command exists to stop.
'=== IDENTITY (fixed for this session) ==='
"inside runner : $(if ($vals['QONTINUI_RUNNER_CONTEXT']) { 'YES' } else { 'NO (or a headless spawn - see note 3)' })"
"runner id     : $(if ($vals['QONTINUI_RUNNER_ID']) { $vals['QONTINUI_RUNNER_ID'] } else { '<unset>' })"
"context       : version $(if ($spawnVer) { $spawnVer } else { '<unparsed>' }) sha $(if ($spawnSha) { $spawnSha } else { '<unparsed>' })"
"briefing      : $briefingRow"
"clause        : $clauseRow"
"tier          : $(if ($vals['QONTINUI_AGENT_TIER']) { $vals['QONTINUI_AGENT_TIER'] } else { '<unset>' })"
"terminal id   : $(if ($vals['QONTINUI_TERMINAL_ID']) { $vals['QONTINUI_TERMINAL_ID'] } else { '<unset>' })"
"worktree mode : $(if ($vals['QONTINUI_AGENT_WORKTREE_MODE']) { $vals['QONTINUI_AGENT_WORKTREE_MODE'] } else { '<unset>' })"
"plans dir     : $(if ($vals['QONTINUI_PLANS_DIR']) { $vals['QONTINUI_PLANS_DIR'] } else { '<unset - optional, plans may live only in the corpus>' })"
"cwd           : $($PWD.Path)"

''
"=== REACHABILITY (now, $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')) ==="
$port = $vals['QONTINUI_RUNNER_API_PORT']; if (-not $port) { $port = '9876' }
# Carry the ROLE alongside the port so the body capture below is tied to the
# probe we MEANT as the runner, not to a `-eq $port` test against a number two
# rows can share. (If a runner genuinely announces 9875 the two rows do describe
# one endpoint - the tag cannot fix that, and the card should be read with the
# announced port in mind.)
$runnerBody = ''
foreach ($probe in @(@{ Role = 'runner'; Port = $port }, @{ Role = 'supervisor'; Port = '9875' })) {
  $p = $probe.Port
  $row = '{0,-10} :{1}' -f $probe.Role, $p
  try {
    $r = Invoke-WebRequest -Uri "http://127.0.0.1:$p/health" -TimeoutSec 20 -UseBasicParsing -ErrorAction Stop
    if ($probe.Role -eq 'runner') { $runnerBody = $r.Content }
    "$row  up (HTTP $($r.StatusCode))"
  } catch {
    # THREE classes, not two. `-ErrorAction Stop` throws on ANY non-2xx, so a
    # runner that is up and answering 503 lands here too - and reporting that as
    # UNKNOWN would collapse "it answered me" into "I have no idea", on the one
    # card whose purpose is not conflating those. curl hands the bash twin the
    # code via `%{http_code}` with no equivalent dance; on 5.1 it hangs off the
    # exception's Response.
    $resp = $_.Exception.Response
    $code = $null
    if ($resp -and $resp.StatusCode) { $code = [int]$resp.StatusCode }
    $st = $_.Exception.Status
    if ($code) { "$row  up (HTTP $code)" }
    elseif ("$st" -eq 'ConnectFailure') { "$row  DOWN (connection refused)" }
    else { "$row  UNKNOWN ($st - not evidence of absence)" }
  }
}
'live coord proxy   not swept (Step 3 has no PowerShell twin - not a verdict)'

# Step 4, from the /health body already fetched above - no second request.
# `data.gitSha`, NOT the top-level `buildId`: see the long note in Step 4 for
# why those are different shas from different build steps. The regex demands
# lowercase hex, so `{"gitSha":null}`, an `unknown` sha, and a runner that
# refused or errored (empty body) all leave $liveSha empty and land on UNKNOWN
# rather than manufacturing a DISAGREE. The `\s*` around the colon is laxer than
# the bash twin's literal match; axum serializes compactly, so no live body
# reaches the difference. `"$runnerBody"` forces a string: `-cmatch` against an
# ARRAY filters instead of matching, and would leave $Matches holding the
# previous capture - a guaranteed false AGREE.
$liveSha = ''
if ("$runnerBody" -cmatch '"gitSha"\s*:\s*"([0-9a-f]+)"') { $liveSha = $Matches[1] }
if (-not $spawnSha -or -not $liveSha) {
  "build cross-check  UNKNOWN (spawn=$(if ($spawnSha) { $spawnSha } else { '?' }) live=$(if ($liveSha) { $liveSha } else { '?' }))"
} elseif ($spawnSha -ceq $liveSha) {
  "build cross-check  AGREE (spawn $spawnSha = live $liveSha)"
} else {
  "build cross-check  DISAGREE - spawned by $spawnSha, talking to $liveSha"
}
```

`ConnectFailure` is the only status that proves nothing is listening; an
answered non-2xx is `up (HTTP <code>)`, exactly as curl's `%{http_code}` renders
it on the bash side; every other status, `Timeout` included, is UNKNOWN.

Two limitations of this block, stated rather than left to be inferred:

- **The cross-check goes UNKNOWN whenever `/health` answers non-2xx.** The body
  is captured only on the success path, and UNKNOWN with no body is honest.
  Bash Step 4 issues its own `curl` without `-f`, so it still parses a `gitSha`
  out of a `503` and returns a real verdict — run Step 4 if you need one from a
  wedged-but-answering runner.
- **`cwd` is spelled differently by the two renders** — msys bash gives the
  POSIX spelling (`/<drive>/<workspace-root>/…`), PowerShell the native one
  (`<Drive>:\<workspace-root>\…`). Same directory; not a disagreement.

## Output shape

Print exactly two labelled blocks, in this order, with the fixed half first:

```
=== IDENTITY (fixed for this session) ===
inside runner : YES
runner id     : <id>
context       : version <v> sha <sha>
briefing      : coord session_briefing/runner-session v<N> | cached v<N> (stale) | builtin-fallback[ (rejected coord v<N>)] | <none - runner predates briefing provenance> | <n/a - no runner context>
clause        : <clause provenance> | <absent - plan-capture dial is off> | <n/a - ...>
tier          : <tier or <unset>>
terminal id   : <uuid>
worktree mode : <mode or <unset>>
plans dir     : <path or <unset>>
cwd           : <path>

=== REACHABILITY (now, <timestamp>) ===
runner  :9876       up (HTTP 200)
supervisor :9875    DOWN (connection refused)
live coord proxy    <path/to/.mcp.json>  (nonce#<fp>) | not swept
build cross-check   AGREE | DISAGREE | UNKNOWN
```

Then one sentence naming anything that came back UNKNOWN and why it is not a
"no". Do not merge the blocks, and do not let a reachability result rewrite an
identity line — that conflation is the whole reason this command exists.
