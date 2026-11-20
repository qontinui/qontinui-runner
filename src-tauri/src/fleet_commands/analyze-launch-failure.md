---
description: "Analyze a Terminal-page pane whose Claude launch failed. Reads the evidence file the runner captured, retries the launch in a NEW pane on the recorded account (never typing into the failed pane), dedupes against prior findings, plans and memory, and ends in exactly one outcome: /pvi for a fixable fleet-repo defect no plan covers, or a coord finding (topic runner-session-launch) for an environmental or transient cause, a retry that succeeded, or a cause a plan or PR already covers. The finding quotes only the failing lines, redacted, and is read back."
argument-hint: "<evidence-path>  (~/.qontinui/launch-evidence/<terminal_id>-<utc>.md)"
allowed-tools: Bash, Read, Grep, ToolSearch, Skill
---

# /analyze-launch-failure — retry a failed launch, then fix it or record it

The runner's "Analyze launch" button opens this command in a fresh pane, with
the path of an evidence file as its argument. That file was captured from a pane
where `claude` was typed into a shell and did not start. The shell stayed alive,
so the pane itself shows no error state.

Arguments: `$ARGUMENTS`

Rules for the whole run:

- **Never type into the failed pane.** Its scrollback is the evidence. A
  retry there would overwrite it, and a second failure would look like the first.
- **Never paste the evidence file anywhere.** It holds raw scrollback, which
  may contain secrets. Quote only the failing lines, redact anything
  token-shaped (`sk-…`, `eyJ…`, long base64 runs, `Bearer …`, `?token=`,
  passwords) through the helper's `redact` verb, and cite the file by its
  path. UUIDs, git shas and sha256 digests are kept on purpose: they are
  evidence, not secrets.
- **An unread answer is UNKNOWN.** A 404, a timeout or an empty body is never
  read as "launched OK" or "no prior record".

Each shell call starts fresh, so every block below sets its own `BASE` from
`${QONTINUI_RUNNER_API_PORT:-9876}`. Responses go to stdout; nothing is written
to a temp file a native `curl` would have to open. The buffer read and the
finding read-back go through the envelope helper the `command-scripts` skill
carries, so a wrong or missing key is a typed UNKNOWN (exit 3) and never an
empty answer; the create response and the launch-failure route are short and
read by eye, and a field you cannot find there is UNKNOWN too. Find it once:

```bash
command_script() {  # print the path of helper $CS_REL, carried by the command-scripts skill
  local d="$PWD" c
  while :; do
    c="$d/.claude/skills/command-scripts/_scripts/$CS_REL"
    [ -f "$c" ] && { printf '%s\n' "$c"; return 0; }
    [ "$d" = / ] && break; d=$(dirname "$d")
  done
  c="$HOME/.claude/skills/command-scripts/_scripts/$CS_REL"
  [ -f "$c" ] && { printf '%s\n' "$c"; return 0; }
  echo "command-scripts: _scripts/$CS_REL not found under $PWD, any parent of it, or $HOME" >&2
  return 1
}
CS_REL=lib/envelope.py command_script
```

Paste the printed path over `<envelope.py path>` in the blocks below. If it is
not found, say so and stop.

If the argument is missing or the file does not exist, say so and stop. The
terminal id is in the file name, before `-<utc>`. If the evidence file names
the terminal id itself, use that instead.

## Step 1 — read and classify the evidence

Read the file with the Read tool. It records the launch command (or
`UNKNOWN (typed by hand)`), the account (a config dir), the pinned session id,
the cwd, the LaunchFailureRegistry entry (or `none recorded`), the pane's
alive/exit state, the runner `buildId`/`gitSha`, and the last 256 KiB of
ANSI-stripped scrollback.

Also read the pane's live registry entry. Phase 2 of the plan adds this route,
so an older runner answers 404. Record that as `launch-failure route: UNKNOWN
(404)`, not as "no failure recorded":

```bash
BASE="http://127.0.0.1:${QONTINUI_RUNNER_API_PORT:-9876}"; TID="<terminal id>"
curl -sS -m 10 -w '\nHTTP %{http_code}\n' "$BASE/terminals/$TID/launch-failure"
```

`{"recorded": false}` (it may be wrapped in `{"success":…,"data":…}`) means the
shim reported nothing. That is not evidence that the launch succeeded.

Pick ONE class from the failing lines and the registry entry. If several match,
use the earliest failure in the scrollback:

| Class | Signals |
|---|---|
| `cli-missing-or-updating` | `command not found`, `ENOENT`, `No such file or directory` naming `claude`, an install or update in progress |
| `shim-gave-up` | registry event `resolve-failed`, `exec-failed` or `launch-failed` |
| `account-limit` | usage or rate limit reached, `limit` with a reset time |
| `account-logged-out` | `/login`, `Invalid API key`, an OAuth or credential refusal, 401 |
| `alias-broken` | the account's launch alias fails to resolve or exec |
| `session-id-in-use` | `already in use` for the `--session-id` |
| `gateway-refused` | 403 or a proxy/gateway refusal from the API |
| `network` | `ECONNREFUSED`, `ENOTFOUND`, `ETIMEDOUT`, TLS errors |
| `no-failure-visible` | the scrollback shows Claude's handshake after all |
| `unclassified` | none of the above |

Keep the 1–5 failing lines you classified from. They, and the registry entry,
are the only scrollback that may leave this machine, and they leave only after
the helper's `redact` verb (`CS_REL=analyze-launch-failure.py command_script`)
has run over them. Use its output, never your own copy, in the finding, the
`/pvi` prompt and any search words:

```bash
SCAN="<analyze-launch-failure.py path>"
python3 "$SCAN" redact <<'EOF_LINES'
<the failing lines and the registry entry>
EOF_LINES
```

## Step 2 — retry in a NEW pane on the recorded account

Take the command from the evidence. If it is `UNKNOWN (typed by hand)`, look in
the scrollback for the typed `claude …` line, or for the account's alias line.
If neither is found, the retry is `not attempted (launch command unknown)`. If
the account is missing, the retry is `not attempted (account unknown)`. Then go
to Step 3.

Open the retry pane. `account` takes a roster name or a full config-dir path.
`initial_command` is typed into the new pane's shell after it starts. Load the
three values with QUOTED heredocs, so a recorded command holding `"`, `$` or
backticks reaches the runner byte for byte and is never expanded here:

```bash
BASE="http://127.0.0.1:${QONTINUI_RUNNER_API_PORT:-9876}"
RETRY_CMD=$(cat <<'EOF_CMD'
<recorded launch command>
EOF_CMD
)
RETRY_ACCT=$(cat <<'EOF_ACCT'
<account from evidence>
EOF_ACCT
)
RETRY_CWD=$(cat <<'EOF_CWD'
<cwd from evidence>
EOF_CWD
)
export RETRY_CMD RETRY_ACCT RETRY_CWD
python3 -c '
import json, os, sys
b = {"initial_command": os.environ["RETRY_CMD"], "account": os.environ["RETRY_ACCT"], "title": "launch retry"}
cwd = os.environ["RETRY_CWD"]
if cwd and os.path.isdir(cwd): b["working_dir"] = cwd
elif cwd: print("cwd gone, retrying in the runner default dir: " + cwd, file=sys.stderr)
print(json.dumps(b))' |
  curl -sS -m 30 -w '\nHTTP %{http_code}\n' -H 'Content-Type: application/json' --data-binary @- "$BASE/terminals"
```

The runner answers 400 to a `working_dir` that no longer exists, so a cwd that
is gone (a removed worktree) is dropped rather than sent. Say so in the retry
line (`cwd gone`), because the retry then ran somewhere else.

Then wait for the handshake, for at most about 60 s (the last poll may finish
a few seconds after the deadline). The buffer is classified by the
`command-scripts` helper `analyze-launch-failure.py scan` (find it as above,
with `CS_REL=analyze-launch-failure.py command_script`). It reads the launch
command from `RETRY_CMD`, attributes only lines after the shell's echo of that
exact command, checks failure and login lines BEFORE the REPL-ready lines, and
redacts the line it quotes. Re-load `RETRY_CMD` the same way, and put the id
from the create response in `RID`:

```bash
BASE="http://127.0.0.1:${QONTINUI_RUNNER_API_PORT:-9876}"; ENV_PY="<envelope.py path>"; SCAN="<analyze-launch-failure.py path>"
RETRY_CMD=$(cat <<'EOF_CMD'
<recorded launch command>
EOF_CMD
)
export RETRY_CMD
RID="<retry pane id>"; verdict=timeout; read_ok=0; end=$((SECONDS + 60))
while [ "$SECONDS" -lt "$end" ]; do
  sleep 2
  left=$((end - SECONDS)); [ "$left" -lt 1 ] && break; [ "$left" -gt 5 ] && left=5
  txt=$(curl -sS -m "$left" "$BASE/terminals/$RID/buffer?format=text" |
        python3 "$ENV_PY" require --door runner-terminal-buffer --key data.data.text --raw) || continue
  read_ok=1
  v=$(printf '%s' "$txt" | python3 "$SCAN" scan) || { verdict=scan-error; break; }
  case "$v" in
    # `noise:` is not final: the REPL can still come up, so keep polling.
    no-echo|""|noise:*) last=$v ;;
    *) verdict=$v; break ;;
  esac
done
[ "$verdict" = timeout ] && case "${last:-}" in noise:*) verdict=$last ;; esac
[ "$verdict" = timeout ] && [ "$read_ok" = 0 ] && verdict=unreadable
[ "$verdict" = timeout ] && [ "${last:-}" = no-echo ] && verdict=no-echo
echo "handshake: $verdict"
```

The `failed:` line is already redacted; quote it as printed, never the raw
buffer.

The create response is enveloped: `{"success": true, "data": <TerminalInfo>}`,
with the new pane's id at `data.id`. The text buffer is nested one level deeper,
at `data.data.text`. A non-2xx create, or an empty id, means the retry is
`not attempted (create refused: <the response's error>)`, with that error text
passed through the helper's `redact` verb first, like every other quoted line.
`claude` was never typed, so a refused create is no evidence about the launch
and never confirms a class. A `409` whose error reads `account at <dir> has no valid credentials (logged
out)` is the account pin's refusal. The runner stamps a status-derived
`code: "CONFLICT"` onto it (`error_detail.context.code_source` is
`status_derived`), so a `code` of `CONFLICT` or none at all, with that text,
is `account-logged-out` evidence; the retry line still reads `not attempted`.
A `409` whose `code` is anything else (`SPAWN_BLOCKED` from the trust gate, a
drain class), whatever its text says about the account, or a `503`, is a
refusal by the runner, not a class.

- `seen`: the retry is `started`. Leave that pane open for the operator, and
  send it nothing. When the retry reused the original pinned `--session-id`,
  that id now belongs to the retry pane: write
  `retry: started (pinned id now held by pane <retry pane id>)`.
- `refused-session-id`: the original pinned `--session-id` is taken. Mint a
  fresh id with `python3 -c 'import uuid; print(uuid.uuid4())'`, substitute it
  for the pinned id in the command, and retry once in ANOTHER new pane. Put
  the substituted command into BOTH `EOF_CMD` heredocs (the create and the
  handshake scan); a scan that still holds the old id never finds its echo and
  reads `no-echo`. Do not type into the refused pane.
- `failed:<line>`: a terminal failure (login or limit screen, missing binary,
  refused credential). The retry is `failed (<line>)`. Leave the pane open as
  a second piece of evidence, and record its id.
- `noise:<line>`: at the deadline the buffer held a failure-shaped line
  (ENOENT, ECONNREFUSED, a TLS error, ...) and no REPL. A slow launch can
  print such a line and still come up, so the retry is
  `UNKNOWN (no handshake in about 60 s; failure-shaped line: <line>)`, never
  `failed`. Quote the line as printed; it is already redacted. Leave the pane
  open and record its id.
- `timeout`: the buffer was readable but showed neither a failure line nor
  the REPL in about 60 s. A slow first launch reads the same, so the retry is
  `UNKNOWN (no handshake and no failure line in about 60 s)`, never `failed`.
  Leave the pane open and record its id.
- `scan-error`: the helper itself failed (a traceback, a missing interpreter).
  The retry result is `UNKNOWN (scan helper failed: <exception type only>)`.
  Never copy its traceback out: it can carry buffer bytes. Leave the pane
  open.
- `unreadable`: no poll returned a readable body, so the retry result is
  UNKNOWN. Record `retry: UNKNOWN (buffer unreadable at
  /terminals/<retry pane id>/buffer)`. Name the door only: the loop keeps no
  status, and the envelope helper's `UNKNOWN:` line can quote raw body bytes,
  so never copy it out. Never record it as `started` or `failed`.
- `no-echo`: the shell never echoed the typed command, so no line of the
  buffer can be attributed to the launch. Record `retry: UNKNOWN (no echo of
  the command)`.

Record exactly one line: `retry: started | failed (<why>) | UNKNOWN (<why>) |
not attempted (<reason>)`, with the retry pane id when there is one. A retry that failed the
same way the original did confirms the class. A retry that started points to a
transient cause.

## Step 3 — look for a prior record

Search all three stores before choosing an outcome. Each search reports a count
or UNKNOWN, and an unreadable store is never counted as "none".

1. **Findings.** `coord_recent_findings` with `topic: "runner-session-launch"`,
   `limit: 50`, and again with `resource_keys` holding the class key
   `runner-session-launch:<class>` and the stems of any plans item 2 finds
   (run item 2 first, or re-run this query once it has answered).
   Read the titles only. Check `available` before trusting an empty list.
2. **Plans.** Search this tenant's plan corpus for the failure class. On the
   Qontinui fleet the known prior plans for this family include
   `2026-10-03-runner-claude-spawn-fails-enoent-during-cli-auto-update`,
   `2026-11-10-migration-holds-for-missing-claude-cli` and
   `2026-11-10-shim-settle-wait-and-in-memory-launch-liveness`; on another
   tenant those stems resolve to nothing, so the full-text query is the search
   that matters. Read each known stem through the runner, and add one
   full-text query for the class:

   ```bash
   BASE="http://127.0.0.1:${QONTINUI_RUNNER_API_PORT:-9876}"
   for s in 2026-10-03-runner-claude-spawn-fails-enoent-during-cli-auto-update \
            2026-11-10-migration-holds-for-missing-claude-cli \
            2026-11-10-shim-settle-wait-and-in-memory-launch-liveness; do
     printf '%s: ' "$s"
     curl -sS -m 20 -w ' HTTP %{http_code}' "$BASE/plan-library/search?kind=plan&slug=$s" | grep -oE "\"slug\": *\"$s\"| HTTP [0-9]+" | tr '\n' ' '; echo
   done
   curl -sS -m 20 -G -w '\nHTTP %{http_code}\n' --data-urlencode "q=<two or three words from the failing line>" \
     "$BASE/plan-library/search?kind=plan&limit=10"
   ```

   Count only rows whose `slug` equals the stem. A page with no such row is
   UNKNOWN, not absent, because an older backend ignores `slug`. A zero from
   the full-text query counts as READ only with `corpus_health` (under `data`)
   beside it showing a live corpus; a runner whose plan sync is off serves a
   frozen corpus, so a zero with no health read, or with a `plan_count` far
   below the plans on `origin/main`, is UNKNOWN. Read a
   matching plan's status and its scope. Then decide whether it covers THIS
   cause, or only a neighbouring one.
3. **Memory.** `coord_memory_search` with `query_text` = the class name plus
   the key words of the failing line, `limit: 20`. Read `live_row_count`
   before concluding there are no hits.

Also search open PRs in the repo that owns the fix site:
`gh pr list --repo <owner/repo> --search "<key words>" --state open`. A non-zero
exit (not authenticated, rate-limited, offline) is UNKNOWN, never "no PR".

## Step 4 — choose exactly ONE outcome

Choose **`/pvi`** only when ALL of these hold:

- the cause is a defect in a fleet-owned repository;
- you can name the fix site (a file and function);
- no finding, plan or open PR from Step 3 covers it, and the findings search,
  the full-text plan search, the memory search and the PR search each READ a
  count. A store that read
  UNKNOWN does not satisfy this condition: post a finding that names the
  UNKNOWN store instead (or re-read it). The by-stem probe of a guessed stem
  is advisory; its UNKNOWN alone does not block.

Run `/pvi <one paragraph: the cause, the failing lines (redacted), the evidence
path, the fix site, and the prior plans you ruled out and why>`. Its outcome
id is the plan path it writes.

Otherwise post a **finding**. This covers an environmental or transient cause
(an account limit, a logged-out account, the network, a CLI auto-update window),
a retry that started and so could not reproduce the failure, and a cause that a
plan or PR already covers. Load the tool with ToolSearch
(`select:mcp__coord-mcp__coord_post_finding`) and call it with:

- `topic`: `"runner-session-launch"`
- `kind`: `"investigation"`
- `title`: one claim, e.g. `Claude launch in runner pane <tid8> failed with
  <class>; retry <result>; covered by <plan|PR> | no code fix`
- `resource_keys`: `["runner-session-launch:<class>", "<covering plan stem or
  owner/repo#n>"]`
- `body`, in this order:
  1. the evidence: the class, the redacted failing lines, the registry entry,
     the runner `buildId`/`gitSha`, the retry line as printed (already redacted) and its pane id, and the
     evidence path (the path only);
  2. the method: this command, plus the doors you read in Step 3 and what
     each one returned;
  3. what a peer should do: cite the covering plan or PR by stem or
     `owner/repo#n`, so this occurrence counts toward it.

When the tool is masked, run the same call over the session's own nonce. Find
`coord-revive.sh` with the same upward walk `command_script` uses (the verb
uses the nearest `.mcp.json` at or above `$PWD`):

```bash
F_TITLE=$(cat <<'EOF_T'
<title>
EOF_T
)
F_BODY=$(cat <<'EOF_B'
<body>
EOF_B
)
F_KEYS='runner-session-launch:<class>,<covering stem or owner/repo#n>'
export F_TITLE F_BODY F_KEYS
FJ=$(python3 -c '
import json, os
keys = [k for k in os.environ["F_KEYS"].split(",") if k]
print(json.dumps({"topic": "runner-session-launch", "kind": "investigation",
                  "title": os.environ["F_TITLE"], "body": os.environ["F_BODY"], "resource_keys": keys}))')
ENV_PY="<envelope.py path>"
CR=""; d="$PWD"
while :; do
  [ -f "$d/.claude/skills/coord-revive/coord-revive.sh" ] && { CR="$d/.claude/skills/coord-revive/coord-revive.sh"; break; }
  [ "$d" = / ] && break; d=$(dirname "$d")
done
[ -n "$CR" ] || { [ -f "$HOME/.claude/skills/coord-revive/coord-revive.sh" ] && CR="$HOME/.claude/skills/coord-revive/coord-revive.sh"; }
[ -n "$CR" ] || { echo "coord-revive.sh not found: the finding is NOT posted (UNKNOWN)"; exit 3; }
ANS=$(bash "$CR" call coord_post_finding "$FJ")
python3 - "$ENV_PY" "$ANS" <<'PY'
import os, sys
sys.path.insert(0, os.path.dirname(sys.argv[1])); import envelope as e
try:
    body = e.load(sys.argv[2], door="coord_post_finding", source="coord-revive call").mcp_body().payload
    print(e.require_first_present(body, ["finding_id", "finding.finding_id"], door="coord_post_finding")[0])
except e.EnvelopeUnknown as x:
    print(x, file=sys.stderr); sys.exit(3)
PY
```

Exit 3 from the helper means the answer could not be read, so the write is
UNKNOWN. Read back before re-posting, since a re-post may duplicate it.

Build the arguments with `json.dumps` as above, never by hand-quoting. If
neither door answers, run `/coord-revive` and follow its verdict. A zero exit
code does not prove the finding was stored.

**Read it back.** Call `coord_recent_findings` with `finding_id` = the returned
id, over the door you used. The outcome counts only when that read returns the
row. If you get a `deduplicated: true` answer, the returned row is your outcome
id.

Leave the evidence file alone; its retention is the runner's.

## Step 5 — final message

End with exactly these four lines, and nothing after them:

```text
class: <class>
retry: started | failed (<why>) | UNKNOWN (<why>) | not attempted (<reason>)   [retry pane <id>]
outcome: finding <finding_id> (read back) | pvi <plan path>   [covers: <stem or PR>]
evidence: <evidence path>
```

If no outcome could be written or read back, the outcome line says
`outcome: UNKNOWN (<door> answered <what>)`. Never claim a finding or a plan
that was not confirmed by a read.
