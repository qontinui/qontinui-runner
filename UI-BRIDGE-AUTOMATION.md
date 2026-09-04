# UI Bridge Automation Primitives (Phase 3I)

This document covers three UI Bridge HTTP endpoints added in Phase 3I so
automation can drive the runner's UI without `page/evaluate` tricks:

| Route                                           | Purpose                                                                         |
| ----------------------------------------------- | ------------------------------------------------------------------------------- |
| `GET /ui-bridge/commands`                       | List allowlisted Tauri commands + JSON schemas                                  |
| `POST /ui-bridge/invoke/{command_name}`         | Invoke an allowlisted Tauri command over HTTP                                   |
| `POST /ui-bridge/control/activate-tab/{tab_id}` | Switch the runner's main tab (or `settings-*` sub-tab) via a native Tauri event |

All routes are exposed on the runner's MCP API port (default `9876`;
secondary runners bind different ports — check `/health` or the
supervisor registry).

---

## `GET /ui-bridge/commands`

Static registry of Tauri commands that are proxyable over HTTP. The
frontend invokes any command returned here on behalf of the HTTP caller
(see `POST /ui-bridge/invoke/...` below). Generic proxy of arbitrary
commands is intentionally NOT supported — only commands on this
allowlist. The list is the single source of truth used by both the
handler and the `/ui-bridge/invoke/...` guard.

### Request

```bash
curl http://localhost:9876/ui-bridge/commands
```

### Response

```json
{
  "success": true,
  "data": [
    {
      "name": "get_web_integration_status",
      "description": "Read the runner's current web-integration settings + registration status.",
      "args_schema": "{\"type\":\"object\",\"properties\":{}}",
      "response_schema": "{\"type\":\"object\", ...}"
    },
    ...
  ]
}
```

`args_schema` and `response_schema` are JSON-Schema strings (embedded as
literals in the Rust binary at `src-tauri/src/ui_bridge_invoke.rs`). The
args schema describes the shape the HTTP caller should send as the
`args` body of `POST /ui-bridge/invoke/...` — which uses JS-land
camelCase keys; Tauri converts those to snake_case for the Rust
command's parameters.

---

## `POST /ui-bridge/invoke/{command_name}`

Invokes a Tauri command on the runner's React side and returns its
result. The handler generates a `request_id`, stores a one-shot sender,
emits a Tauri event `ui-bridge:invoke-request` with
`{ request_id, command, args }`, and waits (default 30s) for the
frontend to emit a matching `ui-bridge:invoke-response` carrying either
`{ ok: true, result: <value> }` or `{ ok: false, error: <string> }`.

### Request body

```json
{ "args": { "fieldA": "...", "fieldB": true } }
```

The `args` object is passed through verbatim to `invoke(command, args)`
on the frontend. Use the camelCase keys the corresponding Tauri command
expects (Tauri converts top-level arg names camelCase → snake_case to
match the Rust signature).

### Query parameters

- `timeoutMs` (optional, u64, default `30000`) — how long to wait for
  the frontend response before returning 504.

### Responses

- `200` — `{ "success": true, "data": <result> }`. `data` is the
  command's return value (or `null` for commands that return `()`).
- `400` — command is not on the allowlist, or args shape is invalid.
- `500` — the frontend reported a command failure (e.g. the Rust
  command returned `Err(...)`). Body:
  `{ "success": false, "error": "<err message>" }`.
- `504` — the frontend didn't respond within the timeout. Body:
  `{ "success": false, "error": "invoke proxy: timed out waiting for frontend response ..." }`.

### Examples

```bash
# 1. Read current web-integration settings.
curl http://localhost:9876/ui-bridge/invoke/get_web_integration_status \
  -X POST -H 'Content-Type: application/json' \
  -d '{"args": {}}'

# 2. Save web-integration settings.
#    Note the JS-style camelCase keys on the args object. Tauri will
#    rename them to snake_case to match the Rust signature.
curl -X POST 'http://localhost:9876/ui-bridge/invoke/save_web_integration_settings' \
  -H 'Content-Type: application/json' \
  -d '{
    "args": {
      "enabled": true,
      "backendUrl": "http://localhost:8000",
      "runnerToken": "qontinui_runner_0000000000000000000000000000000000000000000000000000000000000000"
    }
  }'

# 3. Probe a backend-URL + token pair without persisting anything.
curl -X POST http://localhost:9876/ui-bridge/invoke/test_web_integration_connection \
  -H 'Content-Type: application/json' \
  -d '{
    "args": {
      "backendUrl": "http://localhost:8000",
      "runnerToken": "qontinui_runner_0000000000000000000000000000000000000000000000000000000000000000"
    }
  }'

# 4. Custom timeout (e.g. 10s).
curl -X POST 'http://localhost:9876/ui-bridge/invoke/get_web_integration_status?timeoutMs=10000' \
  -H 'Content-Type: application/json' \
  -d '{"args": {}}'
```

### Adding a new command to the allowlist

Edit `src-tauri/src/ui_bridge_invoke.rs` and add a `ProxyableCommand`
entry to `UI_BRIDGE_COMMANDS`. The name must match the `#[tauri::command]`
function name exactly. Populate both schemas — `GET /ui-bridge/commands`
consumers (tests, SDK generators, tutorial docs) rely on them.

The Tauri command itself must already be registered in
`invoke_handler!` in `src-tauri/src/main.rs`. The HTTP proxy doesn't
register commands — it only dispatches invocations through the frontend.

---

## `POST /ui-bridge/control/activate-tab/{tab_id}`

Switches the runner's main tab (left sidebar nav) to `tab_id` by
emitting a native Tauri event `ui-bridge:activate-tab` with payload
`{ tab_id }`. The frontend's `useAppNavigation` hook subscribes and
calls `setActiveTab(tab_id)`.

### Why this exists alongside `POST /ui-bridge/control/page/set-tab`

Both endpoints flip the same underlying `activeTab` state. Differences:

|                                  | `/page/set-tab`                                     | `/activate-tab/{tab_id}`                                      |
| -------------------------------- | --------------------------------------------------- | ------------------------------------------------------------- |
| Tab id location                  | JSON body `{ "tab": "..." }`                        | URL path segment                                              |
| Transport                        | `page/evaluate` dispatching a JS `CustomEvent`      | Native Tauri event                                            |
| Return                           | Waits ~100ms, reads `[data-page-id]` and returns it | Returns 200 immediately (fire-and-forget)                     |
| Works when webview is slow/stuck | Less reliable (needs JS eval round-trip)            | More reliable (no eval required)                              |
| Settings sub-tab support         | Main-tab only; sub-tab stays on default             | Sub-tab propagates via `TabContent` → `<Settings defaultTab>` |

Prefer `/activate-tab/` for automation; prefer `/page/set-tab` when you
need the post-switch `pageId` readback in a single round-trip.

### Request

```bash
# Switch to the backend-connection settings sub-tab in one call.
curl -X POST http://localhost:9876/ui-bridge/control/activate-tab/settings-backend-connection

# Switch to the main Processes tab.
curl -X POST http://localhost:9876/ui-bridge/control/activate-tab/processes
```

### Responses

- `200` — `{ "success": true, "data": { "success": true, "tab_id": "..." } }`.
- `400` — unknown `tab_id`. Body lists a preview of valid ids and
  points at `src/components/app/tab-types.ts` for the full list.
- `500` — the Tauri event emit failed (unusual — likely a webview
  lifecycle issue).

### Valid `tab_id` values

Anything in the `MainTabId` union in
`src/components/app/tab-types.ts`. The Rust side keeps a parallel
`VALID_TAB_IDS` list in `src-tauri/src/mcp/ui_bridge.rs`; adding a new
tab id requires updating both sides by hand.

`settings-*` ids (e.g. `settings-backend-connection`, `settings-ai`)
activate the main Settings tab and land on the named sub-tab in one
step — the `TabContent` component maps the settings-prefixed id to a
`defaultTab` prop on the `<Settings>` component, which re-syncs its
internal sub-tab state whenever that prop changes.

---

## Reading terminal pane content: `GET /terminals/{id}/buffer?format=text`

xterm renders terminal panes to a **canvas** — UI Bridge
`snapshot`/`read-value`/DOM walks return empty text for them. To verify
what a pane actually shows, do NOT scrape the DOM; read the live
scrollback ring over the runner API instead (same port as the routes
above; this is a runner API route, not a `/ui-bridge/*` one):

```bash
# 1. Find the terminal id (and its page/title/workingDir).
curl http://localhost:9876/terminals

# 2. Read the pane's scrollback as plain text (UTF-8, ANSI/OSC stripped).
curl "http://localhost:9876/terminals/<id>/buffer?format=text"
```

Response shape with `format=text`:

```json
{
  "success": true,
  "data": {
    "data": { "text": "...full scrollback text..." },
    "start_offset": 0,
    "total_bytes_produced": 12345
  }
}
```

Query forms (all on the same handler; `GET /terminals/{id}/output` is an
alias for `/buffer`):

| Query              | Returns                                                                         |
| ------------------ | ------------------------------------------------------------------------------- |
| `?format=text`     | Canonical: UTF-8 lossy decode + ANSI/OSC stripped, nested `{ data: { text } }`   |
| `?decoded=true`    | UTF-8 lossy decode, ANSI escape codes left intact (top-level string `data`)      |
| `?strip_ansi=true` | Implies `decoded=true`; strips CSI/OSC + control bytes (top-level string `data`) |
| _(none)_           | Base64-encoded raw PTY bytes — byte-fidelity for replays / WS scrollback         |

Source: `src-tauri/src/mcp/terminals.rs` (`BufferQuery`,
`get_buffer_handler`, route registration in `routes()`).

## Debug-gated test seams: `POST /ui-bridge/test/*`

These routes exist only in a **debug / `test-fixtures` build**. They are
registered in `src-tauri/src/mcp/test_fixtures.rs::routes()` alone — they are
NOT part of the SDK manifest or `route_entries()`, so the manifest-drift and
SDK-parallel tests do not apply to them.

### StatusStrip seam: `seed-terminal-scenario`

`POST /ui-bridge/test/seed-terminal-scenario` seeds injected sessions into the
*transcript-list* render path. `POST /ui-bridge/test/clear-injected` tears them
down. This seam has no way to exercise the session-RESTORE path — that is what
the lifecycle-store seam below is for.

### Session-restore seam: the lifecycle store

The runner resurrects on-screen sessions at boot by reading its durable
`SessionLifecycleStore`. Testing that path by hand means writing the store's
JSON with exact camelCase keys and computing the anchored-recency offset math
against the wall clock — and one wrong field name used to discard the whole
file, which looks exactly like a passing "restored nothing" run. These three
routes replace that:

| Route | Does |
|---|---|
| `POST /ui-bridge/test/seed-lifecycle-store` | Writes this instance's store from `{"records":[…]}`. `400` on a malformed body |
| `POST /ui-bridge/test/list-lifecycle-open` | Reads it back — the `state == "open"` rows, the restore consumer's input |
| `POST /ui-bridge/test/clear-lifecycle-store` | Empties it — snapshot + sibling WAL + live-store reload |

**Record shape.** Each entry in `records` is camelCase and carries
`sessionId` and `state` (`"open"` / `"closed"`); everything else is optional —
`lastSeenOffsetMs`, `closedAtOffsetMs`, `closeReason`, `pageId`, `zoneIndex`,
`title`, `workingDir`, `confirmedAt`, `restorePendingAt`, `restoreTier`,
`origin`, `terminalId`, `configDir`.
**Every timestamp field on this body is an offset from "now" in millis,
negative = the past** — `confirmedAt` and `restorePendingAt` included, despite
their names — resolved against `Utc::now()` at write time, so a body places a
row at a precise age without the caller knowing the clock.

`confirmedAt` is load-bearing and easy to miss: `confirmed` is one of the two
gates in `is_restorable_identity`, so a row seeded without it reports
`restorable: false` and the frontend's cold-resume path is unreachable from a
seeded store. `terminalId` + `configDir` + `confirmed` are the three fields
`find_confirmed_open_by_terminal` gates on, and `list-lifecycle-open` returns
all three under `open_records` — the only way to assert a seeded BINDING took,
rather than merely a seeded row.

```bash
# 3 restorable open rows, 1 stale "ghost", 1 user-closed row.
curl -sS -X POST http://127.0.0.1:9876/ui-bridge/test/seed-lifecycle-store \
  -H 'Content-Type: application/json' -d '{
    "records": [
      {"sessionId":"alpha","state":"open","zoneIndex":0,"lastSeenOffsetMs":-1000,
       "confirmedAt":-1000},
      {"sessionId":"bravo","state":"open","zoneIndex":1,"lastSeenOffsetMs":-2000,
       "confirmedAt":-2000},
      {"sessionId":"delta","state":"open","zoneIndex":3,"lastSeenOffsetMs":-1200000},
      {"sessionId":"echo","state":"closed","zoneIndex":4,"closeReason":"explicit"}
    ]}'
# -> {"success":true,"seeded":4,"path":"…","reloaded":true,"in_memory_records":4}

curl -sS -X POST http://127.0.0.1:9876/ui-bridge/test/list-lifecycle-open
# -> {"success":true,"open_session_ids":["alpha","bravo","delta"],
#     "open_records":[…],"path":"…","source":"running-store"}
```

**Read `source` on the read-back.** `"running-store"` means the answer came
from the live store this runner is actually using; `"snapshot-file"` means no
store was registered in this process and the file was the whole state. A file
read inside a live runner answers about a store nothing is using — which is
how a `clear` that did not happen used to confirm itself and pass every
clear-then-assert-empty test unconditionally.

**The seed is applied to the RUNNING store, not just the file.** The handler
drops the sibling WAL (whose deltas would otherwise replay over the seed) and
calls `reload_from_disk()`. A registered store that fails to reload answers
**`409`**, never a `success: true` seed that the next persist would overwrite.
`clear` does the same handshake minus the records.

**`seed-lifecycle-store {"records": []}` is a deliberate `400`** — use
`clear-lifecycle-store`. An empty array is far more often a fixture that lost
its rows than a request to wipe the store, and a wipe is destructive and
silent.

**The path is INSTANCE-namespaced, not port-namespaced.** All three routes
resolve `session_lifecycle_store::store_path()` —
`instance::scope_path(<runner dir>)/terminal-sessions.json`: the primary at
`~/.qontinui/runner/`, every secondary under
`~/.qontinui/runner/instance-<name>/`. It has not been port-keyed since
`2026-08-10-temp-runner-session-restore-isolation`, so a recycled temp-runner
port no longer aliases a previous temp's store — but two runners sharing an
INSTANCE NAME would share one file.

Source: `src-tauri/src/mcp/test_fixtures.rs` (`seed_lifecycle_store_handler`,
`list_lifecycle_open_handler`, `clear_lifecycle_store_handler`).

---

---

## Behind the scenes

### Invoke proxy wire flow

```
HTTP caller                Runner (Rust)                   Runner (React)
──────────                 ──────────────                  ───────────────
POST /ui-bridge/invoke/X
{ args: {...} }      ───▶  allowlist check
                           generate request_id
                           register oneshot sender
                           emit Tauri event
                           "ui-bridge:invoke-request"
                           { request_id, command, args } ──▶ listen("ui-bridge:invoke-request")
                           await oneshot (timeout)            invoke(command, args)
                                                              emit Tauri event
                                                              "ui-bridge:invoke-response"
                           listen("ui-bridge:invoke-response") ◀── { request_id, ok, result? | error? }
                           store.deliver(request_id, resp)
                           oneshot resolves
200 { data: result } ◀──
```

### Timeouts and cleanup

- The HTTP handler removes its pending entry if `tokio::time::timeout`
  fires. A late response from the frontend after timeout is a no-op
  (the receiver has been dropped).
- Restart recovery: pending entries are in-memory only. A runner
  restart drops all in-flight invokes; callers retry from scratch.

### Relevant source files

- `src-tauri/src/ui_bridge_invoke.rs` — store, allowlist, types.
- `src-tauri/src/mcp/ui_bridge_invoke_handlers.rs` — HTTP handlers.
- `src-tauri/src/mcp/ui_bridge.rs` — route registration + manifest
  entries (including the activate-tab handler).
- `src-tauri/src/mcp_api.rs` — installs the Tauri
  `ui-bridge:invoke-response` listener that resolves waiting oneshots.
- `src/hooks/useUIBridgeInvokeHandler.ts` — React side that listens
  for `ui-bridge:invoke-request` and dispatches to
  `@tauri-apps/api/core::invoke`.
- `src/components/app/useAppNavigation.ts` — React side that listens
  for `ui-bridge:activate-tab` and flips the active tab.
