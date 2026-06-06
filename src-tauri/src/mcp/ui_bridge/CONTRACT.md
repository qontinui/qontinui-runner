# UI Bridge Route Contract

Reference for agents adding new `/ui-bridge/*` HTTP routes to the runner. The
runner has **no fallthrough proxy**: every contract route the SDK declares must
be wired through explicitly, or callers get a 404. This doc carries the
shape / wrapper / classification info that the live route lists can't.

The two authoritative route registries — *do not duplicate these here, link
and diff against them*:

- **Runner manifest** — `route_manifest()` at `mod.rs:312`, exposed live at
  `GET http://localhost:9876/ui-bridge/_routes`. Concatenates per-family
  `route_entries()` from ~22 submodules.
- **SDK contract** — `UI_BRIDGE_ROUTES: RouteDefinition[]` at
  `ui-bridge/packages/ui-bridge/src/server/types.ts:1182` (190 entries).
  Source of truth for which routes are part of the contract.

## Three implementation classifications

Every `/ui-bridge/*` HTTP route falls into exactly one bucket.

### Runner direct
Handler lives in `qontinui-runner/src-tauri/src/mcp/ui_bridge/<family>.rs`
and is registered via that family's `routes()`, with the static tuple
mirrored in its `route_entries()`. The runner answers the request itself —
typically by calling `ui_bridge_request_sync(...)` into the IPC bridge and
re-shaping the response.

**Pick this when:** the route is part of the documented SDK contract (lives
in `UI_BRIDGE_ROUTES`) and HTTP callers — including agents driving the
runner — need to reach it.

**Canonical example:** `GET /ui-bridge/control/elements`, handler at
`elements.rs:158`, registered in `elements.rs::routes()`, declared in
`elements.rs::route_entries()` at `elements.rs:3047`.

**Files an agent touches:**
- `mcp/ui_bridge/<family>.rs` — handler fn + `routes()` + `route_entries()`
- (optional) `mcp/ui_bridge/routing.rs` — use `add_dual!` for `/control` +
  `/ai` aliasing
- SDK side: `ui-bridge/packages/ui-bridge/src/server/types.ts` +
  `handlers.ts` (or `relay-handlers.ts`)

### Runner outer wrapper
Handler lives in `qontinui-runner/src-tauri/src/mcp/sdk_client.rs`. Path
sits under `/ui-bridge/sdk/<tail>`. The runner forwards over the WS bridge
(`try_ws_dispatch`) or synthesises the response from a snapshot, then falls
back to the SDK's HTTP surface via `sdk_request(...)`.

**Pick this when:** the consumer is a browser-based / WS-transport app that
can't reach the runner's primary `/ui-bridge/control/*` surface directly,
and the route needs the snapshot-synthesis or transport-failover behaviour.

**Canonical example:** `GET /ui-bridge/sdk/component/:id`, `handle_component`
at `sdk_client.rs:1334`. Synthesises the response from `getControlSnapshot`
when the WS dispatcher answers, falls through to `sdk_request` otherwise,
and reuses `build_component_not_found_message` from
`mcp/ui_bridge/component_errors` so the inner `/control/component/:id`
handler and this wrapper emit byte-identical errors (see
`sdk_client.rs:1368-1373`).

**Files an agent touches:**
- `mcp/sdk_client.rs` — handler + `.route(...)` registration in the
  router-builder near `sdk_client.rs:4931`
- (sometimes) `mcp/ui_bridge/component_errors.rs` and the parallel
  `mcp/ui_bridge/<family>.rs` handler, when the two paths must agree on
  error/payload shape

### SDK-only
Declared in `UI_BRIDGE_ROUTES` (`server/types.ts:1182`) but the runner does
NOT expose it. The route only resolves against an SDK-mounted server such
as qontinui-web's `/api/ui-bridge/*` adapter.

**Pick this when:** the route is genuinely useless against the runner — it
operates on SDK-only state with no IPC or snapshot equivalent. Rare. If you
think you need this, reconsider: most of the time you actually want the
runner-direct path with an IPC handler.

**Files an agent touches:** SDK only. The Phase 2a manifest diff
(`sdk_manifest_routes_are_exposed_by_runner`) must be updated to allow-list
the route, otherwise it will fail.

## When adding a new endpoint

1. Add the route to `UI_BRIDGE_ROUTES` in
   `ui-bridge/packages/ui-bridge/src/server/types.ts:1182`. SDK is source
   of truth for the contract.
2. Implement the SDK handler in
   `ui-bridge/packages/ui-bridge/src/server/handlers.ts` (or
   `relay-handlers.ts` if it relays through the WS bridge).
3. If the route should be reachable via the runner's HTTP API:
   - Pick the matching family file under
     `qontinui-runner/src-tauri/src/mcp/ui_bridge/<family>.rs` (or create
     one and wire it into `mod.rs::routes()` + `route_manifest()` at
     `mod.rs:312`).
   - Add the handler fn.
   - Register in the family's `routes()` AND its `route_entries()` — both,
     not one. The `manifest_matches_route_calls` drift test enforces this.
   - If the route should also be reachable under `/ai/<tail>` for
     semantic-search consumers, use the `add_dual!` macro at
     `mcp/ui_bridge/routing.rs:44` instead of two raw `.route(...)` calls.
4. If the route serves browser-based / WS-transport consumers, add a wrapper
   in `qontinui-runner/src-tauri/src/mcp/sdk_client.rs` under
   `/ui-bridge/sdk/<tail>` and register it in the router builder near
   `sdk_client.rs:4931`.
5. Run both contract tests:
   - `cargo test manifest_matches_route_calls` — internal manifest drift
     (`mod.rs:400`).
   - `cargo test sdk_manifest_routes_are_exposed_by_runner` — SDK ↔ runner
     diff (Phase 2a, sibling of `manifest_drift_tests` in `mod.rs`).
6. If the response shape adds fields, update the per-route serializer.
   Field-allow-list patterns silently drop unknown fields — see Wrapper
   gotchas below.

## Wrapper / transformation gotchas

### `serializeComponent` field allow-list
The runner's component serializer in
`src/hooks/ui-bridge-events/utils.ts:94` only emits the fields it knows
about. New SDK fields silently drop on the runner side until that function
adds them — this is what swallowed the `scope` field before
`utils.ts:110` was added. Grep `serializeComponent` and update there
whenever you add a `RegisteredComponent` field. Same applies to
`serializeElement` in the same file.

### Query-parameter parsing is per-handler discrete
The runner reads query params one-by-one in each handler. New SDK query
params are silently ignored unless explicitly added. Example: the
`revealsAny` filter is read at `elements.rs:158`
(`query.get("revealsAny").cloned()`) and applied at `elements.rs:211-226`.
A new `?fooFilter=bar` will not work until a parallel `query.get("fooFilter")`
line lands in the same handler.

### Per-tab routing (`?tabId=`) is a thread-through, not a route
The SDK relay (qontinui-web's `/api/ui-bridge/*`) supports pinning a `/control/*`
command to a specific connected browser tab via `?tabId=<id>` (UI Bridge Item
#4). When two relays/tabs are connected (e.g. two operator machines driving the
same dashboard), the default dispatch targets the relay's `primaryTabId`, which
flips to whichever tab registered last - so a command can route to the wrong
session. `tabId` is **a query param on existing routes, not a new route**, so it
does NOT touch `route_entries()` / the manifest tests; but per the discrete
query-parsing rule above, each `/ui-bridge/sdk/*` wrapper must forward it
explicitly or it silently drops.

Wired today in `sdk_client.rs`:
- `handle_element_action` (`/ui-bridge/sdk/element/:id/action`) - reads
  `tabId`/`targetTabId`/`tab_id`/`target_tab_id` from `SdkActionQueryParams`,
  carries it as `targetTabId` in the WS payload AND as `?tabId=` on the HTTP
  path (`append_tab_id_query`).
- `handle_snapshot` (`/ui-bridge/sdk/snapshot`) - same, for pinning a snapshot
  to a non-primary tab.
- `handle_tabs` (`/ui-bridge/sdk/tabs`) - forwards `?activeOnly=true` /
  `?detailed=true` so callers can discover the live `tabId` (each entry carries
  `tabId`, `isPrimary`, `isActive`, `lastHeartbeat`) before pinning.
- Body-POST wrappers that forward the request body verbatim (`handle_discover`
  for `find`, `handle_page_navigate`, the `clickByText`/`typeInto` convenience
  handlers) need no Rust change: a caller supplying `{"tabId":"..."}` in the
  body is threaded by the SDK's universal `relayCommand` -> `extractTabRouting`.

An unknown/stale `tabId` is rejected by the relay with a structured
`{success:false, code:"TAB_NOT_FOUND"|"TAB_STALE", ...}` envelope (NOT a silent
fall-through to the primary tab); the runner forwards it verbatim.

### Per-window routing (`?windowLabel=`) targets a pop-out runner window
`tabId` (above) pins a command to a remote browser *tab* via the relay.
`windowLabel` is the **local** analog: it targets one of THIS runner process's
own Tauri webview windows — the main window or a pop-out terminal window
(`plans/2026-06-03-runner-popout-terminal-windows.md`). Discover the live labels
via `GET /ui-bridge/control/runner-windows` (each entry carries `label`, `kind`,
`title`); omitting `windowLabel` targets `"main"`, byte-identical to single-window
behavior.

Mechanics (`mcp/ui_bridge/request.rs`): `ui_bridge_request_sync` is the single
chokepoint — it pulls an optional `windowLabel` field out of the request payload
(`split_target_window`), routes the emit to that window's webview, and the
composite `(windowLabel, requestId)` pending key plus the frontend listener's
own-label filter (`useUIBridgeEventHandler.ts`) keep the round-trip unambiguous.
Two thread-through styles, mirroring the `tabId` split:
- **Verbatim-`Value`-body handlers** need no change — a caller including
  `{"windowLabel":"term-1"}` in the body flows straight to the chokepoint
  (e.g. `assert_element`, `batch_actions`, the `ai.rs` handlers).
- **Typed-struct handlers** drop unknown fields, so they read `windowLabel`
  explicitly and re-stamp via `request::target_window_payload`. Wired today:
  `get_elements` (`?windowLabel=`) and `execute_action`
  (`ActionQueryParams.window_label`, which also scopes its pre/post detector and
  disabled-state queries so the element id resolves in the right window's
  registry). Rust callers holding a label call `ui_bridge_request_sync_in_window`.

An unknown `windowLabel` (no such window — e.g. a pop-out that already closed)
returns an immediate, structured error naming the missing window and the
`runner-windows` discovery route, rather than emitting an event every window
filters out by label and making the caller wait the full IPC timeout.

### Status-code mapping is explicit
Default `Json` responses are 200. The SDK's `expect`-style endpoints return
422 on predicate timeout — `ui_bridge_expect_element_handler` at
`elements.rs:2815` returns the predicate result via `ApiResponse` and must
map timeout → 422 itself; the framework will not derive this from the body
shape. Any new SDK endpoint with a non-200 success status needs the same
explicit `(StatusCode, Json<...>)` return tuple.

## Element-id discovery

Element ids are **auto-generated from text content**, not from a stable
component name. An SDK plan that uses `reveals: ["promote-to-worktree-*"]`
will not match a button rendered as `button-promote-this-session-into-an-i-1`.
Globs in `reveals` arrays must be authored against actual registered ids,
not aspirational names.

**Discovery recipe:**

```bash
curl http://localhost:9876/ui-bridge/control/elements | jq '.data[] | .id'
```

Run against the page in question (or a temp runner spawned via the
supervisor — see `qontinui-supervisor/CLAUDE.md` for the spawn-test
payload), then grep for the substring you expect. The id you see in the
response is the canonical id.

`reveals` is a **gating annotation**, not a name pattern.
`button-browse-claude-code-sessions` declares which families it gates
(e.g. `button-session-{id}`, `button-resume-claude---resume-{prefix}-0`).
The gating IDs do NOT have to share the gating button's naming convention,
and a `reveals` glob authored to look like the gate's own id will silently
match nothing.

## Contract test surface

- **Internal manifest drift** — `cargo test manifest_matches_route_calls`
  (`mod.rs:400`). Catches "added a `.route(...)` call, forgot to update
  `route_entries()`" or vice versa within the runner crate.
- **SDK ↔ runner diff** — `cargo test sdk_manifest_routes_are_exposed_by_runner`
  (Phase 2a, sibling of `manifest_drift_tests` in `mod.rs`). Catches "SDK
  added a route, runner returns 404".
- **Behavior smoke** — `scripts/contract-smoke.{sh,ps1}` (Phase 2b).
  Spawns a temp runner, hits each route, asserts response shape and status
  code. Catches the deeper field-stripping / query-param-drop bugs the
  manifest diff can't see.
- **Live manifest endpoint** — `GET http://localhost:9876/ui-bridge/_routes`
  returns the runner's actual registered route list. Useful for
  spot-checking what a running build exposes versus what this doc claims.
