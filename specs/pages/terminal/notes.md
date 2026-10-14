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

## tmux work mode (plan `2026-10-11-runner-terminal-tmux-work-mode`)

States `tmux-one-session-on-screen` and `tmux-session-manager-navigator`,
transitions `enter-tmux-work-mode`, `tmux-swap-session` and
`exit-tmux-work-mode`.

- **Read the mode from the component, not the DOM.** `POST
  /ui-bridge/control/component/terminal-page/action/get-terminal-work-mode`
  returns `{mode, shownTabId, sidebarOpen}`; `set-terminal-work-mode` takes
  `{mode?: "grid" | "tmux"}` (omit to toggle). `get-zone-view-state` reads
  `layoutId: "single"` in tmux mode.
- **"Exactly one pane visible"** is observable per tab: only the shown tab's
  `terminal-input-<tabId>` and `terminal.close-session-<tabId>` have a
  non-zero rect, and there is no `terminal-zone-header-1`. The most recently
  shown off-screen panes stay mounted — up to `TMUX_WARM_TAB_LIMIT` (8, the
  WebGL context cap) besides the shown one; their `terminal-input-<tabId>` is
  registered with a 0×0 rect, so `registeredAt` is unchanged across a swap
  between them. A colder session has no `terminal-input-<tabId>` at all until
  it is shown, when it mounts and replays its scrollback ring.
- **Maximize is refused in tmux mode.** `maximize-zone` and
  `toggle-maximize-zone` fail with an error naming the mode rather than
  reporting `changed: true` for a view tmux already is.
- **A session manager row closes its session** through
  `terminal.session-card-close-<sessionId>` (a row with a live tab in this
  window only) — the way to close an off-screen session without showing it.
- **Output continuity** is checked by comparing the revealed pane's xterm
  buffer (`getScrollback` custom action on `terminal-input-<tabId>`) with the
  server grid (`GET /ui-bridge/sdk/terminal/sessions/<id>/text`), not by a
  DOM text search.
- **`GET /ui-bridge/control/snapshot` can time out in this state.** tmux
  mode opens the session manager, and with a large transcript corpus its
  Live view registers thousands of elements; measured 2026-10-11 on a temp
  runner, the snapshot exceeded its 10 s budget (5,596 registered elements)
  while `POST /ui-bridge/control/find` answered in ~2.4 s and
  `GET /ui-bridge/control/element/<id>` in < 100 ms. Use those two.

## Session organization (plan `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`)

States `session-org-*` cover the Session Manager's group lens, group
headers (expanded / collapsed), Suggest groups, the query box and chips,
the Unknown tail, the search palette, the bulk bar, the Ready-to-close
banner, the close and force-close confirmations, Recently closed, the hover
card, the unseen dot and rail mode. Every transition carries its declared
`effect` in the IR (`state-machine.derived.json`); the bundled projection
drops `effect` by design (`projectIRToBundledPage`), so read the class from
the IR, from this table, or off the element's own `data-effect`.

- **Ids and effects have one source**: `SESSION_ORG_CONTROLS`
  (`src/components/terminal/sessionOrgControls.ts`) and
  `SESSION_QUERY_CONTROLS` (`sessionQueryChips.ts`). Every control renders
  `data-ui-bridge-id` and `data-effect`. Per-row / per-group controls append
  the id: `terminal.session-org.group-header-<groupId>`,
  `…group-collapse-<groupId>`, `…group-menu.close-group-<groupId>`,
  `…rail-item-<sessionId>`, `…unseen-dot-<sessionId>`,
  `…recently-closed.resume-<sessionKey>`, `…query-chip-<chipKey>`.
- **Destructive** (an autonomous walk must not fire them): `close`
  (row menu `session-menu.close`, component `session-org` `close`),
  `close-group` (`group-menu.close-group`, `session-org` `close-group`),
  `close-confirm`, `force-close-confirm`, `bulk.close`,
  `ready-to-close.review`. Close always goes through the confirmation and
  `graceful_exit`, never a kill (D7).
- **Write** (reversible org state, or starts a process): every group-menu
  item but Close group, every session-menu item but Close, the bulk bar's
  Finish / Add to group / New group / Pin / Unpin / Mute / Snooze / Move to
  page / Resume, `recently-closed.resume`, group collapse, `query-save-view`,
  `query-delete-view`, and the component actions `session-org`
  create/rename/recolor/move/ungroup/delete-group, set-group-collapsed,
  add-to/remove-from-group, set-pinned, set-muted, snooze, move-to-page;
  `session-group-suggestions` accept-suggestion / dismiss-suggestion /
  accept-all; `session-search-palette` activate.
- **Read**: chips, query input / clear, pickers, saved-view apply, the
  palette's search / close, Suggest groups (preview only), the rail toggle,
  the resize handle, the hover card, the unseen dot, bulk Clear, close
  Cancel, Force close (arms the second confirmation only), the Include and
  per-session ticks.
- **Drive org changes through `session-org`, not a drag.** Drag-and-drop
  is the operator's path; `POST
  /ui-bridge/control/component/session-org/action/<action>` (or the runner's
  `GET`/`POST /terminal/session-org`) is the deterministic one, and
  `list-groups` reads the store back.
- **Status words are two attributes, not one**: every row carries
  `data-session-location` and `data-session-activity`; `data-session-unseen`
  is `true` / `false` / `unknown` / `untracked`. A query row whose facts are
  unread is listed under the Unknown tail (`data-unknown-reason`), never
  counted as a non-match.
- **Hover card and unseen dot have no runnable jsdom test** on a box without
  `jsdom`; the Phase 8 temp-runner UI Bridge run is their live check.
- **What the UI Bridge can and cannot drive here** (measured on the Phase 8
  temp-runner run, 2026-10-14):
  - Context menus: `rightClick` on a row, and on a group header (which
    declares `GROUP_HEADER_ACTIONS`), opens the row / group menu. Builds
    before qontinui-runner `0c8290a10` refused every right-click on the control
    route. A menu closes when an item is chosen; a bridge `click` elsewhere
    does not dismiss it (it listens for `mousedown`).
  - Drag-and-drop is not advertised on rows or headers, so the `drag` action
    is refused — move sessions with `session-org` `add-to-group` /
    `move-group`.
  - Typing into `terminal.session-org.query-input` with the `type` action
    reports success but leaves the query unchanged (the palette input, with
    the same action list, types fine) — set the query with `session-query`
    `set-query` until that is resolved.
  - The close dialog's buttons are re-registered per phase; after a close the
    result list's **Done** is `terminal.session-org.close-cancel`.
  - Recently closed lives in the **Previous** view, not the Live one.
  - Status of a fixture: an idle `claude` stand-in must draw the `❯` prompt on
    its cursor row, or `graceful_exit` refuses it ("not at an empty prompt")
    and nothing is closed — by design, never a kill.
