# Resume Foreign Session

Resume work from a Claude Code session that ran under a *different account*
on this machine — typically because the original account ran out of tokens.
Reads the JSONL transcript from the parallel account's project dir, extracts
the last N turns, and surfaces the recovered context so this session can
continue the work.

## Arguments

- `$ARGUMENTS` — `<account> [N] [selector]`, all optional:
  - **account** — `hotmail` (default), `gmail`, or any suffix of
    `C:\claude\.claude-<suffix>\`. Pass without the leading `.claude-`.
  - **N** — number of recent turns to extract (default `25`). Decrease
    to save tokens; increase for deeper context.
  - **selector** — one of:
    - **UUID prefix** — 6+ hex chars, all `[0-9a-f]` (e.g. `6f29a17e`).
      Globs the foreign dir for `<prefix>*.jsonl`.
    - **Full path** — contains `\` or `/` or ends in `.jsonl`. Used directly.
    - **`name:<keyword>`** — searches only **session title** fields:
      both user-set names from `/rename` (stored as `type:"custom-title"`
      with a `customTitle` field) and auto-generated names (stored as
      `type:"ai-title"` with `aiTitle`). Best when you used `/rename`
      to give the session a memorable handle. Case-insensitive,
      substring match.
    - **Keyword** — any other string. Grepped (case-insensitive)
      across the **entire transcript content** of every `.jsonl` in
      the foreign project dir. Use when you don't remember the name
      but remember a distinctive symbol, file path, or phrase from
      the session. Words are AND-joined: pass `traffic-light` for one
      term, `"traffic-light auto_register"` (quoted as a single arg)
      for two.

  If selector is omitted, lists the **top 5 candidates by mtime** and
  asks you to pick. With a keyword selector, lists up to **20
  matching candidates by mtime** since the search is already filtered.

Examples:
- `/resume-foreign` — top 5 by mtime from hotmail's qontinui-root sessions
- `/resume-foreign hotmail 30` — top 5 by mtime, will pull 30 turns
- `/resume-foreign hotmail 25 6f29a17e` — direct UUID prefix
- `/resume-foreign hotmail 25 name:traffic-light` — sessions whose
  user-set or auto-generated **title** contains "traffic-light".
  **Best option** when you used `/rename` to name the session.
- `/resume-foreign hotmail 25 traffic-light` — full-content grep
  across all hotmail sessions for "traffic-light". Slower but
  catches sessions that didn't get a custom name.
- `/resume-foreign hotmail 25 "auto_register_file PTY"` — sessions
  that mention BOTH terms in their content (multi-word selector — quote it)
- `/resume-foreign gmail 25 C:\path\to\transcript.jsonl` — explicit path

**Picking a search strategy.**

| You remember | Use |
|---|---|
| The `/rename` you gave the session | `name:<keyword>` |
| A plan filename, symbol, error string, commit SHA from the work | `<keyword>` (full-content grep) |
| The session UUID | `<uuid-prefix>` |
| Nothing specific, just "the recent ones" | (no selector) |

Avoid common words (`fix`, `build`, `runner`) for full-content grep —
they'll match nearly every session. `name:` is more reliable when you
named the session.

## Layout assumption

Each Claude Code account on this machine writes transcripts to
`C:\claude\.claude-<account>\projects\<cwd-slug>\<session-uuid>.jsonl`,
where `<cwd-slug>` is the current cwd with `:\` replaced by `--` and `\`
replaced by `-` (e.g. a cwd of `D:\my-workspace` → `D--my-workspace`).

If the foreign account dir or project subdir doesn't exist, report and
stop — there's nothing to resume.

## Instructions

### 1. Locate candidates

Compute `<cwd-slug>` from `$PWD`. Build the foreign project dir:
`C:\claude\.claude-<account>\projects\<cwd-slug>\`.

Classify the third argument (`selector`) into one of five cases:

**1a — Path (contains `\` or `/` or ends in `.jsonl`).** Use directly.

**1b — UUID prefix (6+ hex chars, all `[0-9a-f]`).** `Glob` the foreign
dir for `<prefix>*.jsonl`. If exactly one match, use it; if multiple,
list with previews and ask the user to disambiguate; if none, error.

**1c — Title search (`name:<keyword>` prefix).** Strip the `name:`
prefix. For each `.jsonl` in the foreign project dir, scan for any
record where `type` is `custom-title` or `ai-title` and check whether
the corresponding field (`customTitle` or `aiTitle`) contains the
keyword (case-insensitive substring). The latest such record per
file wins (titles can change across the session). List matching
files. Multi-word: AND-join across the title text.

```bash
# One-shot extractor — emits "<file>\t<latest-title>" for matches
python - <<'PY'
import json, glob, os, sys
KW = "<keyword(s) lowercased, space-separated>".split()
DIR = r"C:/claude/.claude-<account>/projects/<cwd-slug>"
results = []
for path in glob.glob(os.path.join(DIR, "*.jsonl")):
    latest_title = None
    try:
        with open(path, encoding="utf-8") as f:
            for line in f:
                try: r = json.loads(line)
                except: continue
                t = r.get("type")
                if t == "custom-title":
                    latest_title = ("custom", r.get("customTitle",""))
                elif t == "ai-title" and latest_title is None:
                    latest_title = ("ai", r.get("aiTitle",""))
    except Exception: continue
    if latest_title and all(k in latest_title[1].lower() for k in KW):
        results.append((path, latest_title[0], latest_title[1],
                        os.path.getmtime(path)))
results.sort(key=lambda r: -r[3])
for path, kind, title, mt in results[:20]:
    print(f"{path}\t{kind}\t{title}")
PY
```

**1d — Content keyword (any other non-empty string).** Grep all
`.jsonl` files in the foreign project dir for the keyword,
**case-insensitive**. Use Grep (or a Bash + `grep -liF -- <kw>
*.jsonl` fallback). For multi-word selectors, AND-join: a file must
contain ALL words.

```bash
# Bash equivalent if Grep tool can't traverse jsonl content cleanly:
cd "C:/claude/.claude-<account>/projects/<cwd-slug>"
match_files=()
for f in *.jsonl; do
    hit=1
    for kw in <space-separated keywords>; do
        if ! grep -liF -- "$kw" "$f" >/dev/null 2>&1; then
            hit=0; break
        fi
    done
    [ "$hit" = "1" ] && match_files+=("$f")
done
# Sort by mtime desc:
ls -t "${match_files[@]}" 2>/dev/null | head -20
```

For each match (cases 1c, 1d), render a row containing:
- short UUID (first 8 chars)
- mtime
- size
- **session name** — latest `customTitle` if any (mark with `📛` or
  prefix `[named]`), else latest `aiTitle` (prefix `[ai]`), else
  fall back to first-prompt preview (prefix `[first-msg]`); ≤120 chars
- (1d only) **snippet from the first keyword hit** — surrounding
  ~80 chars, keyword wrapped in `**` for quick scan

Cap at 20 matches. If 20+ files match, mention the truncation and ask
the user to narrow the keyword.

**1e — Selector omitted.** List the **5 most recently modified**
`.jsonl` files in the foreign project dir, each with:
- short UUID (first 8 chars)
- mtime
- size
- session name (same fallback chain as above: customTitle → aiTitle →
  first-prompt preview), ≤120 chars

In all listing cases (1b multi-match, 1c title search, 1d content
keyword, 1e default), render as a numbered list and ask the user
(in plain text — not `AskUserQuestion`) which to resume. Wait for
the answer before continuing. Accept either the list number or a
UUID prefix as the reply.

### 2. Extract the last N turns

The JSONL is one record per line. Schema (verified):
- Top-level `type`: `user`, `assistant`, plus boilerplate
  (`permission-mode`, `file-history-snapshot`, `ai-title`,
  `last-prompt`, `attachment`, `system`) which are NOT conversation turns.
- `user` records: `message.role:"user"`, `message.content` is either a
  string (real user message) or an array containing
  `{type:"tool_result", content:..., tool_use_id:...}` (tool result echo).
- `assistant` records: `message.content` is an array of
  `{type:"text"|"tool_use"|"thinking", ...}`. `thinking` blocks should
  be **skipped** (token-heavy and opaque).

Use Bash + Python (one-shot, no temp file) to extract:

```bash
python - <<'PY'
import json, sys
N = <N>
path = r"<full-path>"
keep = []
with open(path, encoding="utf-8") as f:
    for line in f:
        try:
            r = json.loads(line)
        except json.JSONDecodeError:
            continue
        t = r.get("type")
        if t not in ("user", "assistant"): continue
        msg = r.get("message", {})
        content = msg.get("content", "")
        # Normalize to a list of {role, kind, body} entries
        if isinstance(content, str):
            keep.append({"role": "user", "kind": "text", "body": content})
        elif isinstance(content, list):
            for c in content:
                k = c.get("type")
                if k == "text":
                    keep.append({"role": t, "kind": "text",
                                 "body": c.get("text","")})
                elif k == "tool_use":
                    keep.append({"role": t, "kind": "tool_use",
                                 "tool": c.get("name",""),
                                 "input": c.get("input",{})})
                elif k == "tool_result":
                    body = c.get("content","")
                    if isinstance(body, list):
                        body = "".join(
                            b.get("text","") if isinstance(b, dict) else str(b)
                            for b in body)
                    keep.append({"role": t, "kind": "tool_result",
                                 "body": str(body)})
                # skip "thinking"
# Trim to last N user-or-assistant text turns (boundaries), but
# keep tool_use/tool_result rows in between.
text_count = sum(1 for k in keep if k["kind"] == "text")
target = max(0, text_count - N)
seen = 0
start = 0
for i, k in enumerate(keep):
    if k["kind"] == "text":
        if seen >= target:
            start = i; break
        seen += 1
recent = keep[start:]
# Truncate per-record body sizes
def trunc(s, n): return s if len(s) <= n else s[:n] + "  …[truncated]"
out = []
for k in recent:
    if k["kind"] == "text":
        out.append({"role": k["role"], "kind": "text",
                    "body": trunc(k["body"], 1500)})
    elif k["kind"] == "tool_use":
        # Compact input echo
        try:
            inp = json.dumps(k["input"], ensure_ascii=False)
        except Exception:
            inp = str(k["input"])
        out.append({"role": k["role"], "kind": "tool_use",
                    "tool": k["tool"],
                    "input": trunc(inp, 200)})
    elif k["kind"] == "tool_result":
        out.append({"role": k["role"], "kind": "tool_result",
                    "body": trunc(k["body"], 500)})
print(json.dumps({"turns": out, "raw_count": len(keep),
                  "extracted": len(recent)}))
PY
```

### 3. Render as a context block

Format the extracted records as readable markdown. Alternate user/assistant
turns; for tool_use rows, show `→ [Tool] <tool_name>(<input-preview>)`;
for tool_result rows, show `← <body-truncated>`.

Cap the total context block at ~15K tokens. If the JSON output above
exceeds that budget, drop the *oldest* tool_use/tool_result pairs first
(keep the most recent text-turn boundaries intact).

### 4. Report and ask what's next

After dumping the context, write a short summary in plain text — NOT
the dumped transcript itself, a fresh summary you write:

- One sentence: what was the prior session working on?
- One sentence: what's the apparent in-flight state? (last attempted
  action, last unresolved question, etc.)
- Explicit prompt: *"Continue from here, or pick a different thread?"*

Wait for the user's direction before doing further work. The transcript
ends at the last completed turn — anything mid-tool-use or
post-tool-result that hadn't been integrated is **lost**, and you should
not try to "pick up" by re-running the last-attempted-but-incomplete
action.

## Rules

- **Read-only on the foreign transcript.** Never write to, move, or
  truncate files in `C:\claude\.claude-<account>\`. Treat the foreign
  account's data as untouchable.
- **Token-bound.** Cap the context block at ~15K tokens. If N=25 turns
  produces more, truncate aggressively (drop large tool results, keep
  tool names + first ~200 chars of args). Report what was truncated in
  your summary.
- **No state restoration.** The prior session may have described
  in-progress merges, partial edits, awaiting tool results — that
  ephemeral state is NOT recoverable. Don't re-stage files, re-run
  aborted commands, or assume any uncommitted change described in the
  transcript actually survived to disk. Verify against current cwd
  state before acting.
- **Be explicit about the seam.** In your summary, explicitly say:
  *"The transcript ended at <last-turn-time>. State after that is
  unknown."* This prevents both you and the user from assuming
  continuity that doesn't exist.
- **Don't auto-continue.** Even if the prior session looked like it was
  about to do something obvious, ask the user to confirm before doing
  it. The point of the foreign-session resume is to inform you, not to
  decide for the user.
- **No new files outside the slash-command's own scope.** Don't write
  intermediate transcript dumps, summaries, or notes to disk unless the
  user asks.

## Implementation Notes

$ARGUMENTS
