# Asserting on terminal cell content from automated tests

Terminals in the runner render through xterm.js (canvas/WebGL). Their
visual state is **not** in the DOM, so DOM-search assertions
(`assertionType: "exists"` with a `textContent` criterion) silently
miss whatever a TUI app like Claude Code, vim, or htop draws.

The reliable way to assert on terminal content is to read the
**server-side cell grid** that the runner maintains for every PTY
session. The grid is parsed from PTY bytes by a `vte` parser (the same
one alacritty/wezterm use), so it reflects exactly what the user sees
even for apps that use alt-screen, DEC 2026 synchronized output,
manual cursor positioning, or RGB cell colors.

## Endpoints

All endpoints respond with `{ success: true, data: <body> }`.

| Method | Path | Purpose |
|---|---|---|
| GET | `/ui-bridge/sdk/terminal/sessions/:id/buffer?lines=N` | Last N rendered text rows. |
| GET | `/ui-bridge/sdk/terminal/sessions/:id/grid` | Full cell-level snapshot — `cells[]` with fg/bg/attrs, cursor, title. |
| GET | `/ui-bridge/sdk/terminal/sessions/:id/text` | Compact text view: `lines[]`, `text` (`\n`-joined), cursor row/col, title. Use this for verifier prompts. |
| GET | `/ui-bridge/sdk/terminal/search?q=&regex=&session_id=` | Substring/regex search across one session (`session_id=...`) or all active sessions (omit it). |
| GET | `/ui-bridge/sdk/terminal/sessions/:a/diff/:b` | Row-by-row diff between two terminal grids. |

Tauri-command equivalents (for in-process callers like the verifier):
`terminal_get_grid`, `terminal_grid_text`, `terminal_grid_search`,
`terminal_grid_diff`.

## Test pattern: assert that Claude Code is loaded

```bash
# 1. Get the session id of the first active terminal.
SID=$(curl -s "$RUNNER/ui-bridge/sdk/terminal/sessions" \
  | jq -r '.data[0].session_id')   # adapt to your listing endpoint

# 2. Search the rendered grid for the Claude welcome banner.
curl -s "$RUNNER/ui-bridge/sdk/terminal/search?q=Welcome&session_id=$SID" \
  | jq '.data.totalHits >= 1' \
  || { echo "Claude Code banner missing"; exit 1; }
```

## Test pattern: split-pane parity

```bash
# Two terminals running the same command should render identically.
SID_A=...; SID_B=...
DIFF=$(curl -s "$RUNNER/ui-bridge/sdk/terminal/sessions/$SID_A/diff/$SID_B" | jq '.data.changes | length')
if [ "$DIFF" != "0" ]; then
  echo "Panes diverged"; exit 1
fi
```

## Test pattern: verifier text snapshot

```ts
// In an agentic verification step:
const res = await fetch(`${runner}/ui-bridge/sdk/terminal/sessions/${id}/text`);
const { data: snap } = (await res.json()).data;
const verdict = await llmJudge({
  intent: "Claude Code finished its initial response",
  observed_screen: snap.text,
});
```

## Why not assert through xterm.js directly

The xterm.js JS API exposes `Terminal.buffer.active.getLine(i)`, but
its output is unreliable for any TUI app that uses cursor positioning
or alt-screen — cells written via `\x1b[<row>;<col>H` followed by a
glyph end up in the buffer, but `translateToString(true)` reports the
row as blank because the cursor moved on without filling intermediate
columns. The server-side grid parses these correctly. See
`memory/proj_terminal_grid_snapshot.md` for the full diagnosis.

## When something looks wrong

1. Hit `/grid` first. If `cells` is all default characters, the parser
   tee in `terminal/session.rs::reader_thread` isn't running — fix the
   server before blaming the frontend.
2. If `/grid` looks right but the visible terminal is blank, look at
   `paintGrid` in `TerminalInstance.tsx` and the bootstrap path.
3. Use `/search` as a one-shot existence check; use `/diff` to compare
   before/after action snapshots in agentic flows.
