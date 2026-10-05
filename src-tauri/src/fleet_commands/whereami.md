---
description: "Print this session's context card in two separated blocks: IDENTITY (fixed for the session's lifetime) and REACHABILITY (true only right now). Answers \"am I inside the runner?\" from $QONTINUI_RUNNER_CONTEXT, never from a port probe."
allowed-tools: Read, Glob, Grep, Bash, PowerShell
---

# /whereami — session context card

Print **clearly separated blocks** — two fixed at spawn, one true only now. They
answer different questions and are true for different lengths of time, so they
never share a line:

| Block | Question | Lifetime |
|---|---|---|
| **IDENTITY (fixed)** | Who spawned me, as what, where? | Fixed for this session |
| **SERVED CORPUS (fixed)** | Which `.claude/` was I served, how stale, and is it the build's bundle? | Measured at spawn (Step 1b) |
| **REACHABILITY (now)** | What answers me at this instant? | True only at probe time |

**The identity predicate is `$QONTINUI_RUNNER_CONTEXT` being non-empty — iff.**
The runner injects it at spawn, in `TerminalSession::spawn`
(`qontinui-runner/src-tauri/src/terminal/session.rs`), from the single source
of truth `terminal::runner_context()` (`terminal/mod.rs`), and its first line
is the attributable marker
`[source: qontinui-runner/runner_context@<version>+<git-sha>]`
(`RUNNER_CONTEXT_SOURCE_MARKER`, `terminal/mod.rs`). It is fixed for the
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

**Line 3, where present, is the COORD-CREDENTIAL line** —
`[coord-credential: <posture> since <RFC3339>]`, with `<posture>` one of
`expiring`, `expired`, `absent`, `unrefreshable` or `dark`
(`terminal/mod.rs`, `coord_credential_briefing_line()`; plan
`2026-09-12-runner-loads-with-an-expired-coord-credential-and-tells-nobody`).
It is a statement about **the runner's own coord device JWT**, not about this
session's `.mcp.json`, not about its proxy nonce, and not about coord — all
three can be perfect while this line reads `expired`, and that combination is
the whole reason the line exists. When it is present, every `coord_*` call this
session makes through the runner's forwarder comes back
`401 {"code":"runner_credential_<posture>"}` and the runner drops the matching
`RUNNER_CREDENTIAL_<POSTURE>` reason into `.coord-mcp-status` beside a
perfectly valid `.mcp.json`; the doors that still answer are the ones carrying
their own credential (`/coord-revive`'s L4 and L5), a re-provision fixes
nothing, and a new session fixes nothing either because every session on the box
shares that runner.

**Its ABSENCE is three-way and must never be printed as health.** The runner
emits no line 3 when the posture is `live`, when no refresher pass has concluded
yet (UNKNOWN — a posture printed here would be invented), and on any build
predating the line. This card cannot separate those from the variable alone, so
it reports the absence as exactly that ambiguity — the same reading an absent
`.coord-mcp-status` breadcrumb gets. `expiring` DOES print, unlike the
breadcrumb and the local 401, which fire only when the credential cannot answer
at all.

`GET :9876/health`'s `coordCredential` is what separates the three, and it never
answers `null`: `mcp_api.rs` unwraps a `None` posture into
`{"posture":"unknown","state":"unknown","reason":…}`, so the runner's own
UNKNOWN arrives as the **value** `unknown` rather than as a missing object. An
absent `coordCredential` key therefore means a build predating the field, and
nothing else.

**The SERVED-CORPUS line follows it, and is found by KEY, never by position** —
`[served-corpus: <canonical .claude>] [checkout: …] [bundle: …] [provisioned: …]
[cwd: …]` (plan `2026-09-03-served-corpus-provenance-at-spawn`). The runner
always emits it, so it is line 3 on a spawn with a `live` credential and line 4
when the credential line above is present. Header lines after line 2 are
addressed by their `[key: ` prefix; a positional `sed -n '3p'` would read the
credential line as this one, or this one as the credential line. It says which
`.claude/` tree this session was served, how far that tree's checkout was
behind its LOCAL upstream ref (`origin/main` on this fleet) as of that ref's last fetch, and whether the
files on disk are the spawning build's own bundle — which is how a provisioner
overwrite of tracked source is told apart from a peer's WIP without a git call
of your own. Step 1b renders it; the token contract is in
`knowledge-base/qontinui-specific/runner-development.md` → "The session
briefing" → "Header lines after line 2". A runner built before that plan emits
no such line: report `<none>`, and `<n/a>` when the whole variable is unset.

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
  (`qontinui-supervisor/src/process/env_forwarders.rs:1058`) and the session
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
SPAWN_VER=""; SPAWN_SHA=""; BRIEFING=""; CLAUSE=""; CREDENTIAL=""
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
  # LINE 3 IS OPTIONAL AND PREFIX-MATCHED, never positional. The runner emits it
  # only when there is something to say, so on every healthy spawn - and on any
  # build predating it - line 3 is the FIRST LINE OF THE BRIEFING BODY instead.
  # Taking `sed -n '3p'` as the credential row would print briefing prose as a
  # posture; the `[coord-credential: ` guard is what makes the absence readable.
  LINE3="$(printf '%s\n' "$CTX" | sed -n '3p')"
  case "$LINE3" in
    "[coord-credential: "*) CREDENTIAL="${LINE3#\[coord-credential: }"; CREDENTIAL="${CREDENTIAL%%]*}" ;;
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

# THE ABSENCE IS THE POINT, so it is spelled out rather than left blank. A
# present line names a runner whose coord credential cannot answer; an absent one
# is `live` OR UNKNOWN (no refresher pass has concluded) OR a build that predates
# the line, and this card cannot tell those apart from the variable alone.
# Printing `live` or `ok` here would be the exact fabrication the plan behind
# this row exists to end.
if [ -z "$CTX" ]; then CREDENTIAL_ROW="<n/a - no runner context>"
elif [ -n "$CREDENTIAL" ]; then CREDENTIAL_ROW="$CREDENTIAL  <-- the RUNNER's coord credential cannot answer; your .mcp.json and nonce are FINE. Use /coord-revive's L4/L5 bearer doors; a re-provision, a new session and a runner restart all fix nothing"
else CREDENTIAL_ROW="<absent - live, UNKNOWN (no refresher pass concluded), or a build predating the line. NOT evidence of health: confirm with GET http://127.0.0.1:9876/health .coordCredential>"; fi

if [ -n "$CTX" ]; then INSIDE="YES"; else INSIDE="NO (or a headless spawn - see note 3)"; fi
printf 'inside runner : %s\n' "$INSIDE"
printf 'runner id     : %s\n' "${RUNNER_ID:-<unset>}"
printf 'context       : version %s sha %s\n' "${SPAWN_VER:-<unparsed>}" "${SPAWN_SHA:-<unparsed>}"
printf 'briefing      : %s\n' "$BRIEFING_ROW"
printf 'clause        : %s\n' "$CLAUSE_ROW"
printf 'coord cred    : %s\n' "$CREDENTIAL_ROW"
printf 'tier          : %s\n' "${TIER:-<unset>}"
printf 'terminal id   : %s\n' "${TERMINAL_ID:-<unset>}"
printf 'worktree mode : %s\n' "${WT_MODE:-<unset>}"
printf 'plans dir     : %s\n' "${PLANS_DIR:-<unset - optional, plans may live only in the corpus>}"
printf 'cwd           : %s\n' "$PWD"
```

An unset `QONTINUI_AGENT_TIER` or `QONTINUI_AGENT_WORKTREE_MODE` is normal on an
interactive pane; report it as `<unset>`, not as a tier of zero.

## Step 1b — SERVED CORPUS (fixed at spawn)

Which `.claude/` this session was served, measured by the SPAWNER before the
first turn. It is identity, not reachability: it describes the tree as it stood
at spawn and is not re-measured here — a checkout pulled since then still reads
as it was, and that is the point, because the bodies already expanded into this
session are the ones that tree held.

Tokens, in the runner's order (full contract in `runner-development.md` →
"Header lines after line 2"):

| Token | Says |
|---|---|
| `served-corpus` | canonical path of `<workdir>/.claude` (symlinks resolved) |
| `checkout` | the git checkout holding it: branch@sha, `upstream=` (the LOCAL remote-tracking ref measured against), `behind`/`ahead` of it, `as-of` (the newer of that ref's last `FETCH_HEAD` and its newest reflog entry), and `dirty-claude` (tracked entries only); `none (not a git work tree)` is a stated non-checkout |
| `bundle` | `<N>/<M> identical-to-build <gitSha> stamped=<k> stamped-tracked=<t> dirty-bundle=<d> served: canonical@<sha12> <c> fetched <rfc3339>, builtin <b>, account <a>, unstamped <u>; identical-to-source <i>/<v> unverifiable=<x>`: how many of the M files the spawning binary carries match the disk copy; how many disk copies carry a `qontinui-provenance:` stamp; how many of those stamped files git tracks (`stamped-tracked`); how many bundle-roster paths have tracked changes (`dirty-bundle`); then the `served:` breakdown below. `stamped-tracked` and `dirty-bundle` read `n/a` outside a git work tree and `UNKNOWN(<code>)` — no space, a lowercase-hyphen code — when the count could not be measured, never 0. Codes on both counts (always identical): `no-served-corpus` (no `.claude/`; see `served-corpus`), `checkout-unknown` (git could not locate the checkout; see `checkout`), `outside-work-tree` (the corpus path resolves outside the toplevel git reported). `dirty-bundle` only: `status-unknown` (the checkout's git status was unreadable; see `dirty-claude` in `checkout`). `stamped-tracked` only: `ls-files-failed`, `deadline` (probe budget already spent; git not run), `timed-out` (this `ls-files` run was killed at the budget), `git-unavailable` (git could not be spawned), `output-incomplete` (git exited but its output could not be read in full). A bare `UNKNOWN` is an intermediate build that rendered no reason. Every OTHER token keeps `UNKNOWN (<reason>)` with a space |
| `bundle` → `served:` | The stamped files split by the rung their stamp names, and how many of them still match their SOURCE. `canonical@<sha12> <c>` files are verified only against the canonical snapshot the runner has loaded (`canonical@unloaded <c>`, with no `fetched`, when none is loaded); `builtin <b>` files against this build's bundle; `account <a>` (`served`/`disk_cache`) are never compared; `unstamped <u>` carry no key. `identical-to-source <i>/<v>` is how many of the `<v>` verifiable files match that source, and `unverifiable=<x>` counts files stamped by a different snapshot or build, which this spawn cannot check. A reader cuts the value at `identical-to-build`, ` stamped=` and ` served: `, never by position |
| `provisioned` | `commands written=<w>/<e> skipped-<reason>=<n>… as-of=<ts>; skills written=<w>/<e> skipped-<reason>=<n>… as-of=<ts>`: each provisioner's latest pass for this workdir, units written of units expected and one `skipped-<reason>=<n>` per reason (`git-tracked`, `write-failed`, `unresolved`, `rejected`, `repo-authored`); `UNKNOWN (no provision recorded for this workdir)` when the runner's ledger holds no pass |
| `cwd` | the same checkout probe on the workdir itself, or `same checkout` |

The line is found by KEY in the header run after line 2, never by position (see
the SERVED-CORPUS paragraph above). Each token is cut at its own first `]`, the
same rule as the line-2 parser in Step 1 — a value such as
`UNKNOWN (git timed out after 5s)` carries a space and parentheses but never a
`]`. Every value is printed verbatim, so an `UNKNOWN (<reason>)` token prints
its reason; it is never replaced with a default.

```bash
# Re-derived, not inherited - shell state does not survive between blocks.
CTX="$(printenv QONTINUI_RUNNER_CONTEXT 2>/dev/null)"
SERVED=""
if [ -n "$CTX" ]; then
  # BY KEY, NEVER BY POSITION. The line is 3 on a spawn whose coord credential
  # is `live` and 4 when the conditional `[coord-credential: ` line precedes it,
  # so `sed -n '3p'` is wrong on one of the two shapes. The briefing body opens
  # with prose, so lines 3-4 bound the header run this line can occupy.
  SERVED="$(printf '%s\n' "$CTX" | sed -n '3,4p' | grep -m1 '^\[served-corpus: ')"
fi

echo '=== SERVED CORPUS (fixed at spawn) ==='
CHECKOUT=""; BUNDLE=""; CWD_TOK=""
if [ -z "$CTX" ]; then
  printf 'served corpus : %s\n' '<n/a - no runner context>'
elif [ -z "$SERVED" ]; then
  printf 'served corpus : %s\n' '<none - runner predates served-corpus provenance>'
else
  # One row per `[key: value]` token, each cut at ITS OWN first `]`. Parsing to
  # the end of the line would swallow every later token into the first row.
  REST="$SERVED"
  while :; do
    case "$REST" in *"["*"]"*) ;; *) break ;; esac
    TOK="${REST#*\[}"
    REST="${TOK#*\]}"
    TOK="${TOK%%\]*}"
    KEY="${TOK%%: *}"; VAL="${TOK#*: }"
    [ "$KEY" = "$TOK" ] && VAL="<unparsed token>"
    printf '%-13s : %s\n' "$KEY" "$VAL"
    case "$KEY" in
      checkout) CHECKOUT="$VAL" ;;
      bundle)   BUNDLE="$VAL" ;;
      cwd)      CWD_TOK="$VAL" ;;
    esac
  done
fi

# NAMED inputs, not positionals: a dollar-digit inside an injected body is a
# harness argument placeholder (lint check #18). The helper reads FIELD_SRC /
# FIELD_NAME and writes FIELD_RAW - the value after ` <name>=` verbatim, an
# `UNKNOWN (<why>)` kept whole - and FIELD_OUT, that value when it is a plain
# count and empty otherwise. `${VAR#pattern}` returns the string UNCHANGED on no
# match, which is why presence is tested first and every count shape-guarded.
field() {
  FIELD_RAW=""; FIELD_OUT=""
  case " $FIELD_SRC" in
    *" $FIELD_NAME="*)
      FIELD_RAW=" $FIELD_SRC"; FIELD_RAW="${FIELD_RAW#*" $FIELD_NAME="}"
      case "$FIELD_RAW" in
        "UNKNOWN ("*) FIELD_RAW="${FIELD_RAW%%)*})" ;;
        *) FIELD_RAW="${FIELD_RAW%% *}" ;;
      esac ;;
  esac
  case "$FIELD_RAW" in ''|*[!0-9]*) ;; *) FIELD_OUT="$FIELD_RAW" ;; esac
}
# Why a `stamped-tracked` / `dirty-bundle` value is not a count. Both read `n/a`
# outside a git work tree and `UNKNOWN(<code>)` (no space, so `field` keeps it
# whole) when the count could not be measured - never 0 - and are absent on a
# build predating them. Only the codes that NAME another token point at one; a
# code this reader does not know is printed verbatim, and a bare `UNKNOWN` is an
# intermediate build that rendered no reason.
why_not_counted() {
  case "$FIELD_RAW" in
    '')     WHY="the bundle token carries no $FIELD_NAME field - the spawning build predates it" ;;
    n/a)    WHY="$FIELD_NAME=n/a - the served .claude is not in a git work tree, so nothing in it is tracked source" ;;
    UNKNOWN)
      WHY="$FIELD_NAME=UNKNOWN - the spawning build rendered no reason for it" ;;
    "UNKNOWN("*")")
      CODE="${FIELD_RAW#UNKNOWN(}"; CODE="${CODE%)}"
      case "$CODE" in
        no-served-corpus)  WHY="there is no served .claude/ - see the served-corpus row above" ;;
        checkout-unknown)  WHY="git could not locate the checkout - see the checkout row above" ;;
        outside-work-tree) WHY="the served .claude resolves outside the toplevel git reported" ;;
        status-unknown)    WHY="the checkout's git status was unreadable - see dirty-claude in the checkout row above" ;;
        ls-files-failed)   WHY="git ls-files exited non-zero" ;;
        deadline)          WHY="the git probe budget was already spent; git ls-files was not run" ;;
        timed-out)         WHY="this git ls-files run was killed at the probe budget" ;;
        git-unavailable)   WHY="git could not be spawned" ;;
        output-incomplete) WHY="git exited but its output could not be read in full" ;;
        *)                 WHY="reason code '$CODE' is not one this reader knows" ;;
      esac
      WHY="$FIELD_NAME=$FIELD_RAW - $WHY" ;;
    *)      WHY="$FIELD_NAME=$FIELD_RAW - not a count, n/a or UNKNOWN; this reader does not know the value" ;;
  esac
}

FIELD_SRC="$CHECKOUT"; FIELD_NAME=dirty-claude; field; DIRTY_CLAUDE="$FIELD_OUT"

B_N=""; B_M=""; B_SHA=""
case "$BUNDLE" in
  [0-9]*/[0-9]*" identical-to-build "*)
    B_N="${BUNDLE%%/*}"
    B_M="${BUNDLE#*/}"; B_M="${B_M%% *}"
    case "$B_N$B_M" in *[!0-9]*) B_N=""; B_M="" ;; esac
    B_SHA="${BUNDLE#* identical-to-build }"; B_SHA="${B_SHA%% *}"
    ;;
esac

# The two overwrite verdicts need a MEASURED bundle token; an UNKNOWN one has
# already printed its reason in the row above and supports no verdict at all.
if [ -n "$B_M" ]; then
  FIELD_SRC="$BUNDLE"; FIELD_NAME=stamped; field; STAMPED="$FIELD_OUT"
  # 1. A stamp is written at PROVISION time and canonical sources never carry
  #    one, so a stamped file that git TRACKS is a clobber proven by the file
  #    itself. `stamped-tracked` is exactly that count; nothing else is joined.
  FIELD_NAME=stamped-tracked; field
  if [ -z "$FIELD_OUT" ]; then
    why_not_counted; echo "stamp verdict: none - $WHY"
  elif [ "$FIELD_OUT" -gt 0 ]; then
    echo "PROVISIONER OVERWRITE PROVEN BY STAMP - $FIELD_OUT tracked file(s) carry a provisioner stamp: written by the runner, not edited by a peer"
  elif [ -n "$STAMPED" ] && [ "$STAMPED" -gt 0 ]; then
    echo "stamped=$STAMPED, stamped-tracked=0 - the stamped files are untracked provisions (normal), not a clobber of source"
  fi
  # 2. `dirty-bundle` counts bundle-roster paths with TRACKED changes. When
  #    every bundled file is byte-identical to this build's copy, those changes
  #    ARE the bundle. Dirt outside the roster is not provisioner output at all.
  FIELD_SRC="$BUNDLE"; FIELD_NAME=dirty-bundle; field
  if [ -z "$FIELD_OUT" ]; then
    why_not_counted; echo "overwrite verdict: none - $WHY"
  elif [ "$FIELD_OUT" -gt 0 ] && [ "$B_N" = "$B_M" ]; then
    echo "PROVISIONER OVERWRITE SIGNATURE - the $FIELD_OUT dirty bundled file(s) are byte-identical to build $B_SHA's bundle - a provisioner write, not peer WIP"
  elif [ "$FIELD_OUT" -eq 0 ] && [ -n "$DIRTY_CLAUDE" ] && [ "$DIRTY_CLAUDE" -gt 0 ]; then
    echo "dirty-claude=$DIRTY_CLAUDE, dirty-bundle=0 - the tracked changes under .claude are OUTSIDE the bundle (a settings render, peer WIP, ...), not provisioner output"
  fi
fi
# 3. `behind` is counted against the LOCAL remote-tracking ref the token names
#    as `upstream=`, which the spawn path never fetches. Print that ref's age
#    beside the count, so a stale ref is never read as current drift (it may be
#    further behind now, or not at all).
for PAIR in "checkout|$CHECKOUT" "cwd|$CWD_TOK"; do
  WHICH="${PAIR%%|*}"; T="${PAIR#*|}"
  FIELD_SRC="$T"; FIELD_NAME=behind; field; BEHIND="$FIELD_OUT"
  if [ -n "$BEHIND" ] && [ "$BEHIND" -gt 0 ]; then
    FIELD_NAME=upstream; field; UPSTREAM="${FIELD_RAW:-its upstream}"
    FIELD_NAME=as-of; field; ASOF="${FIELD_RAW:-<no as-of>}"
    echo "$WHICH is $BEHIND commit(s) behind $UPSTREAM AS OF $ASOF - the local ref's age, not a live count"
  fi
done
```

A `bundle` of `<M` on a FRESH checkout is expected, not an alarm: the canonical
side is claude-config, so a checkout ahead of the spawning build differs from
its bundle. Read `bundle` beside `checkout`'s `behind`/`ahead`, never alone.

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

# The dev-only supervisor. The product has none, so this row is CONDITIONAL:
# probe it only when QONTINUI_RUNNER_ID is set, which the supervisor stamps on
# every runner it spawns. Unset (outside any runner, or under one no supervisor
# spawned) - print n/a, never DOWN, since nothing was expected to listen.
if [ -n "$(printenv QONTINUI_RUNNER_ID 2>/dev/null)" ]; then
  PROBE_URL="http://127.0.0.1:9875/health"; probe
  printf 'supervisor (dev)  %s\n' "$PROBE_VERDICT"
else
  printf 'supervisor (dev)  n/a - not probed: QONTINUI_RUNNER_ID is unset (no supervisor-spawned runner started this session; the product ships no supervisor)\n'
fi
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
# Your workspace root: $WORKSPACE_ROOT wins ($QONTINUI_ROOT is accepted too);
# else the first directory, from the MAIN checkout upward, that holds sibling
# repo checkouts - a child whose `.git` is a DIRECTORY (a linked worktree's
# `.git` is a file). The main checkout comes from --git-common-dir, NOT
# --show-toplevel, which inside a linked worktree names the worktree container
# and makes this sweep probe nothing. Else the main checkout's parent; else $PWD.
ROOT="${WORKSPACE_ROOT:-${QONTINUI_ROOT:-}}"
if [ -z "$ROOT" ]; then
  GC="$(git rev-parse --git-common-dir 2>/dev/null)"
  [ -n "$GC" ] && GC="$(cd "$GC" 2>/dev/null && pwd)"
  if [ -n "$GC" ]; then
    MAIN="$(dirname "$GC")"; D="$MAIN"
    while [ -n "$D" ] && [ "$D" != "/" ] && [ "$D" != "." ]; do
      for c in "$D"/*/.git; do [ -d "$c" ] && { ROOT="$D"; break 2; }; done
      D="$(dirname "$D")"
    done
    [ -z "$ROOT" ] && ROOT="$(dirname "$MAIN")"
  fi
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
# spelling of a `/<drive>/.../.mcp.json` path; the same call with `cygpath -w`
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

⚠️ **Probe a second, independent instance before you name a cause.** This whole
block is a *reachability* measurement, and reachability is the half of this card
that is true only right now. "13 files swept, 0 LIVE", "curl exit 7", "HTTP
401", "`buildId` X ≠ the spawn-time one" are measurements and are always
reportable. *"the runner is gone"*, *"coord is down"*, *"this build predates the
route"*, *"I am not inside the runner"* are **mechanisms** — and this command
exists precisely because those four keep getting inferred from a port probe that
answers a different question. Identity does not flip when a port does.

**Rung 1 — unconditional, credential-free.** Ask a second, independent instance
of the door before naming why the first one failed:

```bash
curl -sS -o /dev/null -w 'coord  %{http_code}\n' -m 10 \
  "${COORD_HTTP_URL:-https://coord.qontinui.io}/coord/agent-prompt-documents"
curl -sS -o /dev/null -w 'runner %{http_code}\n' -m 10 http://127.0.0.1:9876/health
```

A `401` from coord is a **pass**: the deployment is up and serving `/coord/…`,
so a proxy sweep with nothing LIVE is a *nonce* verdict about this box, never a
coord outage. `/health` answering at all refutes "the runner is gone" whatever
the sweep said — and its `buildId` is the only thing that turns "this build
predates the route" from a guess into a measurement (Step 4 below is that
comparison; run it rather than asserting it). Spell the runner probe
`127.0.0.1`, never `localhost`: the runner binds the IPv4 loopback only and
`localhost` pays a doomed `::1` connect first — measured 2026-08-03,
`127.0.0.1` 2133 ms, `[::1]` 2057 ms failing, `localhost` 4047 ms = the sum.

**Rung 2 — wherever a coord door answers.** Call **`coord_recent_findings`**
with `topic: "coord-transport"`, or the `resource_keys` you were about to act
on, before reporting any fleet-wide cause: a nonce eviction or a door outage
that hit this box has usually hit peers, and `coord.findings` is
pull-by-relevance — nothing pushes a finding at you, so a session that never
asks is told nothing. HTTP twin when the tool is masked or its transport is
dead: `GET $COORD_HTTP_URL/coord/agent-findings?topic=…&resource_keys=…`; the
two filters are **OR'd, not AND'd**, so passing both WIDENS the read. Read
`available` **before** `count` — `available: false` is UNKNOWN, not "nothing was
filed" [policy: `verification-and-evidence` `silent-empty-is-unknown`].

If neither instance answers, that is **UNKNOWN**, not confirmation of the local
mechanism: two silent doors are two silent doors. Print the row as UNKNOWN and
name both probes you actually ran.

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

# THE LIVE HALF OF THE CREDENTIAL ROW, from the body already fetched - no second
# request. The IDENTITY block reports what the runner said at SPAWN; this reports
# what it says NOW, and they differ in both directions (a credential that expired
# after spawn, or one that has since healed).
#
# DO NOT ASSUME A KEY ORDER INSIDE THE OBJECT. `CoordCredentialStatus::to_json`
# builds it with `serde_json::json!`, and serde_json is pinned at 1.0.149 with
# NO `indexmap` dependency, so `preserve_order` is off and its `Map` is a
# `BTreeMap`: the keys serialize SORTED, and `posture` is nowhere near the
# front. A pattern anchored on `{"posture":` reads NOTHING from a real body, and
# the first cut of this block did exactly that; it looked like it worked only
# because the UNKNOWN body carries just posture/reason/state, which sorts
# `posture` first.
#
# DELIBERATELY NO KEY LIST HERE. One stood in this comment for a day and was
# already stale: `attributable` landed in `CoordCredentialStatus` and, sorting
# ahead of `canAnswer`, moved `posture` from ninth of thirteen to tenth of
# fourteen. A transcribed roster of a sibling repo's field names is the drift
# class this fleet keeps paying for, and no checker reaches this one -- so what
# is written down is the PROPERTY the reader depends on, which does not move:
# the object's fields are all SCALARS (string, bool, integer or null), so the
# first `}` after the key IS the object's close and bounds the read, and within
# that bound the FIRST `"posture":"` is the value regardless of how many keys
# precede it or what they are called. A new scalar key cannot break this;
# measured against the fourteen-key body on 2026-09-16.
LIVE_CRED=""
CRED_PRESENT=""
case "$LIVE_BUILD" in
  *'"coordCredential":'*)
    CRED_PRESENT=1
    CRED_SEG="${LIVE_BUILD#*\"coordCredential\":}"
    CRED_SEG="${CRED_SEG%%\}*}"
    case "$CRED_SEG" in
      *'"posture":"'*)
        LIVE_CRED="${CRED_SEG#*\"posture\":\"}"
        LIVE_CRED="${LIVE_CRED%%\"*}"
        ;;
    esac
    ;;
esac
# THREE absences, and each names only what it can. `posture: "unknown"` is a
# VALUE here, not an absence: the runner never serves `coordCredential: null` --
# `mcp_api.rs` unwraps a `None` posture into {"posture":"unknown","state":
# "unknown","reason":...}, so UNKNOWN arrives through the row above and prints
# as `unknown`.
if [ -z "$LIVE_BUILD" ]; then
  printf 'coord cred (now)   UNKNOWN - /health did not answer; nothing here observed the credential\n'
elif [ -n "$LIVE_CRED" ]; then
  printf 'coord cred (now)   %s\n' "$LIVE_CRED"
elif [ -n "$CRED_PRESENT" ]; then
  # The key IS present - the case above established it - so "the build predates
  # the field" is the ONE explanation ruled out on this path, and printing it
  # would be the fabrication this row exists to prevent.
  printf 'coord cred (now)   UNKNOWN - coordCredential is present but this reader could not parse a posture out of it (shape changed, or the body was truncated)\n'
else
  printf 'coord cred (now)   UNKNOWN - no coordCredential key in this /health body (build predates the field)\n'
fi

# THE RUNNER'S DEFAULT TENANT, from the same body. `activeTenantPin` is the
# runner's three-way verdict on `machine.json::active_tenant_id` (plan
# `2026-09-17-findings-carry-a-triage-stamp-and-the-steward-reads-since-last-run`
# Phase 5): `pinned` carries `activeTenantId`, `unpinned` is the legitimate
# single-tenant shape, and `unresolvable` is NOT "unset" - the file is missing,
# unreadable, not JSON, or its value is not a UUID. It is what a
# `/findings-steward` cycle without `--tenant` runs against, and it is CONTEXT
# for Step 5, never this session's tenant. An absent key is a build predating
# the field.
LIVE_PIN=""
LIVE_TENANT=""
PIN_PRESENT=""
case "$LIVE_BUILD" in *'"activeTenantPin"'*) PIN_PRESENT=1 ;; esac
case "$LIVE_BUILD" in
  *'"activeTenantPin":"'*)
    LIVE_PIN="${LIVE_BUILD#*\"activeTenantPin\":\"}"
    LIVE_PIN="${LIVE_PIN%%\"*}"
    ;;
esac
case "$LIVE_BUILD" in
  *'"activeTenantId":"'*)
    LIVE_TENANT="${LIVE_BUILD#*\"activeTenantId\":\"}"
    LIVE_TENANT="${LIVE_TENANT%%\"*}"
    ;;
esac
if [ -z "$LIVE_BUILD" ]; then
  printf 'default tenant     UNKNOWN - /health did not answer\n'
else
  case "$LIVE_PIN" in
    pinned)       printf 'default tenant     pinned %s (machine.json::active_tenant_id)\n' "${LIVE_TENANT:-<unparsed>}" ;;
    unpinned)     printf 'default tenant     unpinned (single-tenant; a --tenant-less /findings-steward reads its own door: default_source=session-binding)\n' ;;
    unresolvable) printf 'default tenant     unresolvable - machine.json is missing, unreadable, not JSON, or active_tenant_id is not a UUID; a --tenant-less /findings-steward STOPS UNKNOWN\n' ;;
    '')
      # Present-but-unparsed is NOT "the build predates the field" -- the same
      # split the coord cred row above makes.
      if [ -n "$PIN_PRESENT" ]; then
        printf 'default tenant     UNKNOWN - activeTenantPin is present but this reader could not parse a value out of it\n'
      else
        printf 'default tenant     UNKNOWN - no activeTenantPin key in this /health body (build predates the field)\n'
      fi ;;
    *)            printf 'default tenant     UNKNOWN - activeTenantPin=%s is not a value this reader knows\n' "$LIVE_PIN" ;;
  esac
fi

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

## Step 5 — TENANCY: which tenant each half of this session acts as

A session has **three** tenants, decided by three different mechanisms, and they
can disagree (plan
`2026-09-10-spawn-tenant-never-reaches-the-session-coord-credential` P0; served
by a runner build carrying qontinui-runner PR #1558):

| Half | What decides it | Field in the runner's census |
|---|---|---|
| **row** | the tenant the spawn picker / `--tenant` stamped on the session | `tenancy.row.tenantId` |
| **data plane** | the tenant the runner's own work-scoped coord writes present | `tenancy.dataPlane` |
| **credential** | the tenant this session's coord-mcp key actually selects — where its memory, prompt-document and gate writes land | `tenancy.credential` |

A session labelled B whose credential resolves to A writes to A and gets a
`201` for it; this card is where that is visible. Read it from the runner's own
census, `GET /control/sessions/info`, for the entry whose `identity.terminalId`
is `$QONTINUI_TERMINAL_ID`, and print what it says **verbatim**, reasons
included — never pick one tenant as "the" tenant.

`divergence` is the comparison verdict: `diverged`, `agree`, or `unknown` (a
tenant-less session whose spawn-time device default was not recorded — after a
runner restart on a build that does not persist it — cannot be compared, and
`unknown` is NOT agreement). Which fields answer depends on the runner BUILD:

- a build carrying the runner P2/P3 change (same plan) serves `divergence`,
  `row.spawnDeviceDefaultStatus` and `row.spawnDeviceDefaultReason`;
- a build carrying qontinui-runner PR #1558 (P0/P1) but not that change serves
  the `tenancy` block with only the boolean `diverged`, whose `false` also covers
  "could not compare" — so the card reports
  `unknown (runner predates the divergence field)`, never `agree`;
- a build with neither serves **no `tenancy` block at all**: the whole tenancy
  row is UNKNOWN (absent field), never agreement.

This is a REACHABILITY-class read (it needs the runner up right now) about an
IDENTITY-class fact, so it prints under its own header, after both blocks.

```bash
# Re-derived - shell state does not survive between Bash tool calls (Step 2).
API_PORT="$(printenv QONTINUI_RUNNER_API_PORT 2>/dev/null)"
TERM_ID="$(printenv QONTINUI_TERMINAL_ID 2>/dev/null)"
echo '=== TENANCY (read from the runner now) ==='
if [ -z "$TERM_ID" ]; then
  echo 'tenancy        UNKNOWN - $QONTINUI_TERMINAL_ID is unset, so the census cannot name this session (not a runner-spawned terminal, or a headless spawn)'
  exit 0
fi
BODY="$(mktemp)"; trap 'rm -f "$BODY"' EXIT
# Native curl.exe / python.exe open the path themselves, so hand them the
# Windows spelling where cygpath exists (MSYS_NO_PATHCONV - see Step 3).
BODYP="$BODY"
command -v cygpath >/dev/null 2>&1 && BODYP="$(cygpath -w "$BODY")"
CODE="$(curl -s --connect-timeout 3 -m 20 -o "$BODYP" -w '%{http_code}' "http://127.0.0.1:${API_PORT:-9876}/control/sessions/info" 2>/dev/null)"
RC=$?
if [ "$RC" = "7" ]; then echo 'tenancy        UNKNOWN - runner DOWN (connection refused)'; exit 0; fi
if [ "$RC" != "0" ] || [ "$CODE" != "200" ]; then
  echo "tenancy        UNKNOWN - census did not answer 200 (curl exit $RC, HTTP ${CODE:-none}); not evidence of anything"
  exit 0
fi
if command -v jq >/dev/null 2>&1; then
  jq -r --arg term "$TERM_ID" '
    def v(x): if x == null then "<null>" else (x | tostring) end;
    if (.data | type) != "object" then "tenancy        UNKNOWN - census envelope has no data object"
    elif .data.status != "ok" then "tenancy        UNKNOWN - census unavailable: \(v(.data.reason))"
    else ([.data.sessions[]? | select(.identity.terminalId? == $term)] | first) as $s
    | if $s == null then "tenancy        UNKNOWN - terminal \($term) is not in the census (not an OPEN session on this runner)"
      elif ($s.tenancy | type) != "object" then "tenancy        UNKNOWN - no tenancy block (runner build predates it; absent field, NOT agreement)"
      else $s.tenancy as $t
      | "row            tenant \(v($t.row.tenantId))  spawn-default \(v($t.row.spawnDeviceDefaultTenantId)) [\(v($t.row.spawnDeviceDefaultStatus)) \(v($t.row.spawnDeviceDefaultReason))]  current-default \(v($t.row.currentDeviceDefaultTenantId)) (context only)",
        "data plane     \(v($t.dataPlane.status))  tenant \(v($t.dataPlane.tenantId))  reason \(v($t.dataPlane.reason))",
        "credential     \(v($t.credential.status))  tenant \(v($t.credential.tenantId))  slot \(v($t.credential.slot))  reason \(v($t.credential.reason))",
        "slot posture   \(v($t.credential.posture.status))  value \(v($t.credential.posture.value))  canAnswer \(v($t.credential.posture.canAnswer))  reason \(v($t.credential.posture.reason))",
        (if ($t | has("divergence")) then "divergence     \($t.divergence)"
         else "divergence     unknown (runner predates the divergence field; its diverged=\(v($t.diverged)) cannot say \"could not compare\")" end)
      end
    end' < "$BODY"  # envelope-ok: a predicate search over the census; every absent key prints <null> or a named UNKNOWN line, never an inferred tenant
else
  PY="$(command -v python3 || command -v python)"
  if [ -z "$PY" ]; then echo 'tenancy        UNKNOWN - neither jq nor python can read the census (LOCAL fault, not a verdict)'; exit 0; fi
  TERM_ID="$TERM_ID" "$PY" -c 'import json,os,sys
d=json.load(open(sys.argv[1]))  # envelope-ok: every absent key below prints a named UNKNOWN line or <null>, never an inferred tenant
v=lambda x: "<null>" if x is None else str(x)
data=d.get("data") if isinstance(d,dict) else None
if not isinstance(data,dict): print("tenancy        UNKNOWN - census envelope has no data object"); sys.exit(0)
if data.get("status")!="ok": print("tenancy        UNKNOWN - census unavailable: %s" % v(data.get("reason"))); sys.exit(0)
rows=[r for r in (data.get("sessions") or []) if isinstance(r,dict) and (r.get("identity") or {}).get("terminalId")==os.environ["TERM_ID"]]
if not rows: print("tenancy        UNKNOWN - terminal %s is not in the census" % os.environ["TERM_ID"]); sys.exit(0)
t=rows[0].get("tenancy")
if not isinstance(t,dict): print("tenancy        UNKNOWN - no tenancy block (runner build predates it; absent field, NOT agreement)"); sys.exit(0)
g=lambda o,k: (o or {}).get(k)
row,dp,cr=t.get("row"),t.get("dataPlane"),t.get("credential")
po=g(cr,"posture")
print("row            tenant %s  spawn-default %s [%s %s]  current-default %s (context only)" % (v(g(row,"tenantId")),v(g(row,"spawnDeviceDefaultTenantId")),v(g(row,"spawnDeviceDefaultStatus")),v(g(row,"spawnDeviceDefaultReason")),v(g(row,"currentDeviceDefaultTenantId"))))
print("data plane     %s  tenant %s  reason %s" % (v(g(dp,"status")),v(g(dp,"tenantId")),v(g(dp,"reason"))))
print("credential     %s  tenant %s  slot %s  reason %s" % (v(g(cr,"status")),v(g(cr,"tenantId")),v(g(cr,"slot")),v(g(cr,"reason"))))
print("slot posture   %s  value %s  canAnswer %s  reason %s" % (v(g(po,"status")),v(g(po,"value")),v(g(po,"canAnswer")),v(g(po,"reason"))))
print("divergence     %s" % (t["divergence"] if "divergence" in t else "unknown (runner predates the divergence field; its diverged=%s cannot say \"could not compare\")" % v(t.get("diverged"))))' "$BODYP"  # envelope-ok: the same predicate search as the jq arm
fi
```

Read the rows, do not summarise them away: a `credential` tenant that differs
from the `row` tenant is the wrong-tenant-write condition itself, and
`current-default` is context only (so is Step 4's `default tenant` row: the
runner's `activeTenantPin`, what a `--tenant`-less `/findings-steward` cycle
runs against, never this session's tenant) — after an operator switches the device
default every running session legitimately differs from it. An `unknown`
anywhere names its reason; carry the reason into the one-sentence summary.

## Fallback when bash hangs

msys `bash` has been observed hanging on this box where PowerShell works — switch
rather than retrying. This one block carries the **whole** card: Step 1's IDENTITY
rows, Step 1b's SERVED CORPUS rows and cross-checks, Step 2's port probes and
Step 4's build cross-check.

**What it does NOT carry is Step 3's proxy sweep, nor Step 5's tenancy read** — that sweep needs a JSON
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
$spawnVer = ''; $spawnSha = ''; $briefing = ''; $clause = ''; $served = ''
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
  # SERVED CORPUS, by KEY in lines 3-4 and never by position - Step 1b's
  # `sed -n '3,4p' | grep -m1 '^\[served-corpus: '`. `StartsWith(…, Ordinal)`,
  # not `-clike`: `[` opens a wildcard character class in a -like pattern.
  foreach ($l in @($ctxLines | Select-Object -Skip 2 -First 2)) {
    $l = $l.TrimEnd("`r")
    if ($l.StartsWith('[served-corpus: ', [StringComparison]::Ordinal)) { $served = $l; break }
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
'=== SERVED CORPUS (fixed at spawn) ==='
$checkoutTok = ''; $bundleTok = ''; $cwdTok = ''
if (-not $vals['QONTINUI_RUNNER_CONTEXT']) { 'served corpus : <n/a - no runner context>' }
elseif (-not $served)                      { 'served corpus : <none - runner predates served-corpus provenance>' }
else {
  # One row per token, each cut at ITS OWN first `]` - the bash loop in Step 1b.
  foreach ($m in [regex]::Matches($served, '\[([^\]]*)\]')) {
    $tok = $m.Groups[1].Value
    $i = $tok.IndexOf(': ', [StringComparison]::Ordinal)
    if ($i -lt 0) { $key = $tok; $val = '<unparsed token>' }
    else          { $key = $tok.Substring(0, $i); $val = $tok.Substring($i + 2) }
    '{0,-13} : {1}' -f $key, $val
    if ($key -ceq 'checkout') { $checkoutTok = $val }
    elseif ($key -ceq 'bundle') { $bundleTok = $val }
    elseif ($key -ceq 'cwd') { $cwdTok = $val }
  }
}
# The three cross-checks of Step 1b, with the same predicates. Get-ServedRaw is
# the bash `field` helper: the value after ` <name>=` verbatim, an
# `UNKNOWN (<why>)` kept whole, or '' when the field is absent.
function Get-ServedRaw([string]$t, [string]$name) {
  if (" $t" -cmatch (' ' + [regex]::Escape($name) + '=(UNKNOWN \([^)]*\)|\S*)')) { $Matches[1] } else { '' }
}
function Get-ServedNum([string]$t, [string]$name) {
  $raw = Get-ServedRaw $t $name
  if ($raw -cmatch '^[0-9]+$') { [int]$raw } else { $null }
}
# The bash `why_not_counted`, code for code: `UNKNOWN(<code>)` carries its
# reason; only the codes that NAME another token point at one; an unknown code
# is printed verbatim; a bare `UNKNOWN` is a build that rendered no reason.
$notCountedCodes = @{
  'no-served-corpus'  = 'there is no served .claude/ - see the served-corpus row above'
  'checkout-unknown'  = 'git could not locate the checkout - see the checkout row above'
  'outside-work-tree' = 'the served .claude resolves outside the toplevel git reported'
  'status-unknown'    = "the checkout's git status was unreadable - see dirty-claude in the checkout row above"
  'ls-files-failed'   = 'git ls-files exited non-zero'
  'deadline'          = 'the git probe budget was already spent; git ls-files was not run'
  'timed-out'         = 'this git ls-files run was killed at the probe budget'
  'git-unavailable'   = 'git could not be spawned'
  'output-incomplete' = 'git exited but its output could not be read in full'
}
function Get-NotCountedWhy([string]$name, [string]$raw) {
  if (-not $raw)         { "the bundle token carries no $name field - the spawning build predates it" }
  elseif ($raw -ceq 'n/a') { "$name=n/a - the served .claude is not in a git work tree, so nothing in it is tracked source" }
  elseif ($raw -ceq 'UNKNOWN') { "$name=UNKNOWN - the spawning build rendered no reason for it" }
  elseif ($raw -cmatch '^UNKNOWN\((.*)\)$') {
    $code = $Matches[1]
    # A PowerShell hashtable matches keys case-INSENSITIVELY; the bash `case`
    # does not, so the key is found with -ceq to keep the twins identical.
    $hit = @($notCountedCodes.Keys | Where-Object { $_ -ceq $code })
    if ($hit.Count -eq 1) { $what = $notCountedCodes[$hit[0]] }
    else { $what = "reason code '$code' is not one this reader knows" }
    "$name=$raw - $what"
  }
  else                   { "$name=$raw - not a count, n/a or UNKNOWN; this reader does not know the value" }
}
$dirtyClaude = Get-ServedNum $checkoutTok 'dirty-claude'
if ($bundleTok -cmatch '^([0-9]+)/([0-9]+) identical-to-build (\S+)') {
  $bN = $Matches[1]; $bM = $Matches[2]; $bSha = $Matches[3]
  $stamped = Get-ServedNum $bundleTok 'stamped'
  $stampedTracked = Get-ServedNum $bundleTok 'stamped-tracked'
  if ($null -eq $stampedTracked) {
    "stamp verdict: none - $(Get-NotCountedWhy 'stamped-tracked' (Get-ServedRaw $bundleTok 'stamped-tracked'))"
  } elseif ($stampedTracked -gt 0) {
    "PROVISIONER OVERWRITE PROVEN BY STAMP - $stampedTracked tracked file(s) carry a provisioner stamp: written by the runner, not edited by a peer"
  } elseif ($null -ne $stamped -and $stamped -gt 0) {
    "stamped=$stamped, stamped-tracked=0 - the stamped files are untracked provisions (normal), not a clobber of source"
  }
  $dirtyBundle = Get-ServedNum $bundleTok 'dirty-bundle'
  if ($null -eq $dirtyBundle) {
    "overwrite verdict: none - $(Get-NotCountedWhy 'dirty-bundle' (Get-ServedRaw $bundleTok 'dirty-bundle'))"
  } elseif ($dirtyBundle -gt 0 -and $bN -ceq $bM) {
    "PROVISIONER OVERWRITE SIGNATURE - the $dirtyBundle dirty bundled file(s) are byte-identical to build $bSha's bundle - a provisioner write, not peer WIP"
  } elseif ($dirtyBundle -eq 0 -and $null -ne $dirtyClaude -and $dirtyClaude -gt 0) {
    "dirty-claude=$dirtyClaude, dirty-bundle=0 - the tracked changes under .claude are OUTSIDE the bundle (a settings render, peer WIP, ...), not provisioner output"
  }
}
foreach ($pair in @(@('checkout', $checkoutTok), @('cwd', $cwdTok))) {
  $behind = Get-ServedNum $pair[1] 'behind'
  if ($null -ne $behind -and $behind -gt 0) {
    $upstream = Get-ServedRaw $pair[1] 'upstream'; if (-not $upstream) { $upstream = 'its upstream' }
    $asOf = Get-ServedRaw $pair[1] 'as-of'; if (-not $asOf) { $asOf = '<no as-of>' }
    "$($pair[0]) is $behind commit(s) behind $upstream AS OF $asOf - the local ref's age, not a live count"
  }
}

''
"=== REACHABILITY (now, $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')) ==="
$port = $vals['QONTINUI_RUNNER_API_PORT']; if (-not $port) { $port = '9876' }
# Carry the ROLE alongside the port so the body capture below is tied to the
# probe we MEANT as the runner, not to a `-eq $port` test against a number two
# rows can share. (If a runner genuinely announces 9875 the two rows do describe
# one endpoint - the tag cannot fix that, and the card should be read with the
# announced port in mind.)
$runnerBody = ''
# The dev-only supervisor row is CONDITIONAL, as in the bash twin: probed only
# when QONTINUI_RUNNER_ID shows a supervisor spawned this runner.
$probes = @(@{ Role = 'runner'; Port = $port })
if ($vals['QONTINUI_RUNNER_ID']) { $probes += @{ Role = 'supervisor'; Port = '9875' } }
foreach ($probe in $probes) {
  $p = $probe.Port
  # Row labels match the bash twin exactly: `runner  :<port>`, `supervisor (dev)`.
  $row = if ($probe.Role -eq 'runner') { "runner  :$p" } else { 'supervisor (dev)' }
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
if (-not $vals['QONTINUI_RUNNER_ID']) { 'supervisor (dev)  n/a - not probed: QONTINUI_RUNNER_ID is unset (no supervisor-spawned runner started this session; the product ships no supervisor)' }
'live coord proxy   not swept (Step 3 has no PowerShell twin - not a verdict)'
'tenancy            not read (Step 5 has no PowerShell twin - UNKNOWN, not agreement)'

# The live credential row, from the body already fetched - the pwsh twin of the
# bash block above, with the same three absences and the same refusal to assume
# a key order. `[^}]*` is the regex spelling of that bash block's "bound the read
# to the object": serde_json serializes this object's keys SORTED (BTreeMap --
# 1.0.149, no `indexmap`, so `preserve_order` is off), so `posture` arrives deep
# inside the object and an anchor on `\{\s*"posture"` matches nothing on a real
# body. The bound holds for the reason the bash comment gives -- every field is
# a scalar, so no nested `}` can end it early -- and NOT because of any
# particular key list, which is why neither comment carries one.
$liveCred = ''
$credPresent = "$runnerBody" -cmatch '"coordCredential"\s*:'
if ("$runnerBody" -cmatch '"coordCredential"\s*:\s*\{[^}]*"posture"\s*:\s*"([^"]*)"') { $liveCred = $Matches[1] }
if (-not "$runnerBody") {
  'coord cred (now)   UNKNOWN - /health did not answer; nothing here observed the credential'
} elseif ($liveCred) {
  "coord cred (now)   $liveCred"
} elseif ($credPresent) {
  'coord cred (now)   UNKNOWN - coordCredential is present but this reader could not parse a posture out of it (shape changed, or the body was truncated)'
} else {
  'coord cred (now)   UNKNOWN - no coordCredential key in this /health body (build predates the field)'
}

# The runner's default tenant, from the same body - see the bash twin in Step 4
# for what the three pins mean.
$livePin = ''; $liveTenant = ''
if ("$runnerBody" -cmatch '"activeTenantPin"\s*:\s*"([^"]*)"') { $livePin = $Matches[1] }
if ("$runnerBody" -cmatch '"activeTenantId"\s*:\s*"([^"]*)"') { $liveTenant = $Matches[1] }
if (-not "$runnerBody") {
  'default tenant     UNKNOWN - /health did not answer'
} elseif ($livePin -ceq 'pinned') {
  "default tenant     pinned $(if ($liveTenant) { $liveTenant } else { '<unparsed>' }) (machine.json::active_tenant_id)"
} elseif ($livePin -ceq 'unpinned') {
  'default tenant     unpinned (single-tenant; a --tenant-less /findings-steward reads its own door: default_source=session-binding)'
} elseif ($livePin -ceq 'unresolvable') {
  'default tenant     unresolvable - machine.json is missing, unreadable, not JSON, or active_tenant_id is not a UUID; a --tenant-less /findings-steward STOPS UNKNOWN'
} elseif (-not $livePin -and ("$runnerBody" -cmatch '"activeTenantPin"')) {
  'default tenant     UNKNOWN - activeTenantPin is present but this reader could not parse a value out of it'
} elseif (-not $livePin) {
  'default tenant     UNKNOWN - no activeTenantPin key in this /health body (build predates the field)'
} else {
  "default tenant     UNKNOWN - activeTenantPin=$livePin is not a value this reader knows"
}

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
  POSIX spelling (`/<drive>/<dir>/…`), PowerShell the native one
  (`<Drive>:\<dir>\…`). Same directory; not a disagreement.

## Output shape

Print the labelled blocks in this order — the two fixed-at-spawn blocks first,
then REACHABILITY, then the TENANCY block from Step 5:

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

=== SERVED CORPUS (fixed at spawn) ===
served-corpus : <canonical .claude path> | UNKNOWN (<reason>)
checkout      : <repo> <branch>@<sha12> upstream=<ref> behind=<n> ahead=<m> as-of=<ts> dirty-claude=<k> | none (not a git work tree) | UNKNOWN (<reason>)
bundle        : <N>/<M> identical-to-build <gitSha> stamped=<k> stamped-tracked=<t|n/a|UNKNOWN(<code>)> dirty-bundle=<d|n/a|UNKNOWN(<code>)> served: canonical@<sha12|unloaded> <c>[ fetched <ts>], builtin <b>, account <a>, unstamped <u>; identical-to-source <i>/<v> unverifiable=<x> | UNKNOWN (<reason>)
provisioned   : commands written=<w>/<e> [skipped-<reason>=<n> ...] as-of=<ts>; skills written=<w>/<e> [skipped-<reason>=<n> ...] as-of=<ts> | UNKNOWN (no provision recorded for this workdir)
cwd           : <repo> <branch>@<sha12> behind=<n> as-of=<ts> dirty=<m> | same checkout | UNKNOWN (<reason>)
[PROVISIONER OVERWRITE PROVEN BY STAMP - ... | stamped=<k>, stamped-tracked=0 - ... | stamp verdict: none - <why>]
[PROVISIONER OVERWRITE SIGNATURE - ... | dirty-claude=<k>, dirty-bundle=0 - ... | overwrite verdict: none - <why>]
[<which> is <n> commit(s) behind <upstream> AS OF <ts> - ...]
(or the single row `served corpus : <none - runner predates served-corpus provenance> | <n/a - no runner context>`)

=== REACHABILITY (now, <timestamp>) ===
runner  :9876       up (HTTP 200)
supervisor (dev)    DOWN (connection refused) | n/a - not probed: QONTINUI_RUNNER_ID is unset (no supervisor-spawned runner started this session; the product ships no supervisor)
live coord proxy    <path/to/.mcp.json>  (nonce#<fp>) | not swept
build cross-check   AGREE | DISAGREE | UNKNOWN
default tenant      pinned <uuid> | unpinned (...) | unresolvable - <why> | UNKNOWN - <why>

=== TENANCY (read from the runner now) ===
row            tenant <uuid|<null>>  spawn-default <uuid|<null>> [<recorded|unknown> <reason>]  current-default <uuid> (context only)
data plane     <owned|device|unresolved|unknown>  tenant <uuid|<null>>  reason <reason|<null>>
credential     <resolved|unknown>  tenant <uuid|<null>>  slot <tenant|default|<null>>  reason <reason|<null>>
slot posture   <observed|unknown>  value <live|expiring|expired|...>  canAnswer <bool>  reason <reason|<null>>
divergence     diverged | agree | unknown[ (why)]
```

or a single `tenancy  UNKNOWN - <reason>` line when the census cannot be read,
the terminal is not in it, or the runner predates the block.

Then one sentence naming anything that came back UNKNOWN and why it is not a
"no". Do not merge the blocks, and do not let a reachability result rewrite an
identity line — that conflation is the whole reason this command exists.
