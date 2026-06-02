# TerminalPage

## Asserting on terminal cell content (Claude Code, vim, htop, etc.)

DOM-search assertions in this spec (`assertionType: "exists"` with a
`textContent` criterion) **only see the runner's own UI chrome** — tab
bars, buttons, status indicators. They do **not** see what a TUI app
draws inside an xterm pane, because xterm.js renders to a canvas /
WebGL surface that's not in the DOM.

To assert on terminal cell content, query the server-side cell grid:

| Use case | Endpoint |
|---|---|
| Is text "Welcome" visible in any terminal? | `GET /ui-bridge/sdk/terminal/search?q=Welcome` |
| Is text "Welcome" in this specific session? | `GET /ui-bridge/sdk/terminal/search?q=Welcome&session_id=<id>` |
| Get the rendered text rows of a session | `GET /ui-bridge/sdk/terminal/sessions/<id>/buffer` |
| Get full cell-level grid (fg/bg/attrs/cursor) | `GET /ui-bridge/sdk/terminal/sessions/<id>/grid` |
| Compact text view for verifier prompts | `GET /ui-bridge/sdk/terminal/sessions/<id>/text` |
| Diff two sessions row-by-row | `GET /ui-bridge/sdk/terminal/sessions/<a>/diff/<b>` |

Full reference + worked examples:
[`src/components/terminal/GRID_TESTING.md`](../../../src/components/terminal/GRID_TESTING.md).
End-to-end smoke at `scripts/test-grid-endpoints.sh`.

The assertions in `spec.uibridge.json` below cover the runner UI
shell. Cell content lives on the HTTP side because the spec runner's
DOM-search primitive can't reach it.

## Spec maintenance

- Removed spec state `term-plan-cli-script` (2026-06-02): it asserted on plan-CLI *script output*, not page elements — a category error for a UI-Bridge page spec. The behavior it nominally covered is exercised by the Rust integration test `src-tauri/src/flywheel_e2e_tests.rs`.
