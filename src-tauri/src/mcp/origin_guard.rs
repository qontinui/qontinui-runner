//! The browser-origin guard for the runner's loopback HTTP API.
//!
//! Plan `2026-09-17-runner-loopback-api-accepts-any-origin`. Binding to
//! `127.0.0.1` keeps other machines out, but a web page running on the
//! operator's own machine is ALREADY on loopback: before this module every page
//! the operator's browser loaded could `fetch('http://127.0.0.1:9876/…')`, and
//! `Access-Control-Allow-Origin: *` let it read the reply — a coord device JWT
//! from `POST /ui-bridge/invoke/get_coord_device_token`, a `.mcp.json` nonce
//! from `GET /files/read`, a terminal's stdin over `/terminals/{id}/ws`.
//!
//! # What this guard changes, and what it deliberately does not
//!
//! **Invariant (the autonomy discriminator):** a request with no `Origin`
//! header, no cross-site Fetch Metadata and a loopback `Host` behaves exactly
//! as before. Every agent, script and MCP client — curl, PowerShell,
//! `requests`, `reqwest`, the Claude Code HTTP MCP client — is such a request.
//! The guard changes what a *browser origin* is evidence of; it adds no
//! credential, header or configuration an agent must hold.
//!
//! Two checks, in order, on every request that reaches a registered route:
//!
//! 1. **Host gate.** `Host` must name `127.0.0.1`, `localhost` or `[::1]` with
//!    the port the server ACTUALLY bound (read per request — the router is
//!    built before the bind and the bind may fall back to `port + 1/2`), or a
//!    value in [`ENV_ALLOWED_HOSTS`]. An absent `Host` is admitted (HTTP/1.0;
//!    no browser omits it). This is the DNS-rebinding control: a rebound page
//!    is same-origin with its target and sends no `Origin` at all, so only the
//!    `Host` it names tells it apart from curl. Refusal: 403
//!    [`CODE_HOST_NOT_LOOPBACK`].
//! 2. **Origin classification** into [`OriginClass`], then a verdict keyed on
//!    the router's own `MatchedPath` (so the guard and the router cannot
//!    disagree about which route a raw path hit). A preflight is judged by the
//!    method it names in `Access-Control-Request-Method`, so it is admitted
//!    exactly when the real request would be.
//!    - `NonBrowser` and `FirstParty` reach every route.
//!    - `Trusted`, `Extension` and `Foreign` never reach a [`CREDENTIAL_DOORS`]
//!      route: a door returns a secret, reads a caller-named path, makes a
//!      server-side request to a caller-chosen URL (which would launder a
//!      browser request into a NonBrowser one), or spawns/drives a process.
//!      The one transitional exception is [`TRUSTED_DOOR_GRACE`] — doors the
//!      qontinui-web dev frontend calls today. Under the default
//!      [`RoutePolicy::EnforceDoors`] it admits ONLY the built-in
//!      [`DEFAULT_TRUSTED_ORIGINS`] (the dev frontends it was justified for),
//!      metered as `graceAdmitted` on `/health`. Stated plainly: until
//!      qontinui-web #1380 deploys and the grace is removed, those default dev
//!      origins RETAIN local command execution (hooks and their `sh -c` test,
//!      shell-command run, backup import), arbitrary file reads and
//!      caller-directed server-side requests through the graced doors.
//!      Operator-added Trusted origins (env / settings) get every door
//!      refused.
//!    - `Trusted` is local trust for NON-door routes: [`TRUSTED_ROUTES`] holds
//!      app features that run workflows and checks, because that is what the
//!      web dev frontend is for. List only origins trusted like the runner.
//!    - [`EXTENSION_ONLY_ROUTES`] (the DOM element inventory) are reachable
//!      from the webview and extension pages only — never from a web page's
//!      origin, so the ui-bridge extension's content-script fallback loses
//!      them. The Extension class is decided by SCHEME, not tied to the
//!      ui-bridge extension's ID: any installed extension's own pages qualify.
//!      An extension request that carries no `Origin` and no cross-site Fetch
//!      Metadata (e.g. host-permission fetches reporting
//!      `Sec-Fetch-Site: none`) classifies as NonBrowser — a residual of
//!      granting an extension host permissions, not something headers can
//!      distinguish.
//!    - Where the route policy enforces for their class, browser classes
//!      additionally reach only [`FOREIGN_ROUTES`] (all browser classes) and
//!      [`TRUSTED_ROUTES`] (Trusted) — a TOTAL allowlist, so a route added
//!      tomorrow is refused to browsers by default (Phase 2; the same
//!      reasoning that turned `relay_path_policy` from a denylist into an
//!      allowlist). Where it shadows, the verdict is computed, admitted,
//!      logged at WARN once per origin+route and counted; `off` skips it.
//!
//! # Known non-browser paths and residuals
//!
//! - A rathole-tunnelled request (`/tunnel/start`) keeps the tunnel server's
//!   `Host`; the Host gate admits that hostname while the tunnel is
//!   registered. A tunnelled request with no `Origin` therefore has
//!   NonBrowser (full local) access — the tunnel's reach is the tunnel
//!   server's access control, not this guard's. The hostname stays registered
//!   until `/tunnel/stop` or the next `/tunnel/start`, even if the child dies;
//!   a stale entry admits only a `Host` naming the tunnel server, which a
//!   rebinding page cannot produce without controlling that server's DNS.
//! - `GET /graphql` (GraphiQL) is served by `async_graphql::GraphiQLSource`,
//!   which loads its scripts from a CDN without SRI; those scripts run as the
//!   runner's own loopback origin (FirstParty). The generator exposes no SRI
//!   hook, so this is recorded rather than fixed here.
//!
//! The guard is middleware on the upgrade request, so it runs BEFORE axum's
//! `WebSocketUpgrade` extractor — CORS never covered WebSockets; this does.
//! A refused preflight never reaches the CORS layer or a handler.
//!
//! CORS itself ([`cors_layer`]) no longer answers `*`: it echoes the exact
//! origin the guard admitted (tower-http adds `Vary: Origin`), and grants
//! Private-Network / Local-Network-Access preflights to `FirstParty` and
//! `Trusted` only.
//!
//! # Kill switches (read once at spawn — never restart a runner to apply one)
//!
//! - [`ENV_GUARD`]`=0` turns BOTH checks off (CORS still echoes the exact
//!   origin rather than `*`). Absent or any other value leaves them on.
//! - [`ENV_ROUTE_POLICY`] = `enforce` | `enforce-foreign` | `enforce-doors` |
//!   `shadow` | `off`. Default [`DEFAULT_ROUTE_POLICY`] (`enforce-doors`) —
//!   see its doc.
//!
//! Extra trusted origins come from [`ENV_ALLOWED_ORIGINS`] (headless path,
//! read at spawn) and the settings field [`SETTINGS_FIELD`] (re-read without a
//! restart). `/health` reports all of it as `originGuard`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Router;
use serde_json::{json, Value};
use tower_http::cors::{AllowOrigin, AllowPrivateNetwork, Any, CorsLayer};

/// `0` disables the whole guard (Host gate and origin checks).
pub const ENV_GUARD: &str = "QONTINUI_RUNNER_ORIGIN_GUARD";
/// `enforce` | `enforce-foreign` | `enforce-doors` | `shadow` | `off`.
pub const ENV_ROUTE_POLICY: &str = "QONTINUI_RUNNER_ORIGIN_ROUTE_POLICY";
/// Comma list of extra origins admitted as [`OriginClass::Trusted`].
pub const ENV_ALLOWED_ORIGINS: &str = "QONTINUI_RUNNER_ALLOWED_ORIGINS";
/// Comma list of extra `Host` header values the Host gate admits.
pub const ENV_ALLOWED_HOSTS: &str = "QONTINUI_RUNNER_ALLOWED_HOSTS";
/// The runner settings field holding extra trusted origins.
pub const SETTINGS_FIELD: &str = "api.allowed_origins";

pub const CODE_HOST_NOT_LOOPBACK: &str = "HOST_NOT_LOOPBACK";
pub const CODE_CROSS_ORIGIN_REFUSED: &str = "CROSS_ORIGIN_REFUSED";

/// The route-policy default when [`ENV_ROUTE_POLICY`] is unset:
/// **`enforce-doors`** — credential doors refused to every non-first-party
/// browser origin (with the Trusted [`TRUSTED_DOOR_GRACE`] logged instead),
/// and the non-door route allowlists SHADOWED for every browser class.
///
/// Why not the plan's `enforce`, nor `enforce-foreign`:
///
/// - qontinui-web production builds default their runner base URL to
///   `http://127.0.0.1:9876`, and whether deployed `https://app.qontinui.io`
///   pages actually call a loopback runner is UNMEASURED (Phase 0 live
///   capture). Enforcing the Foreign allowlist could break a shipped product
///   path; shadowing it meters exactly that question on `/health`.
/// - qontinui-web's `runner-proxy` Origin strip (plan F4) landed as
///   `5c3533f07`; its deploy is not measured. Until it is, the co-located
///   proxy hop (local dev, the demo box) forwards the end user's origin.
/// - The doors are what carry the exposure (tokens, files, execution), and no
///   browser caller other than the dev frontend's graced list uses one.
///
/// Graduation: `enforce-foreign`, then `enforce`, once the live readings exist.
pub const DEFAULT_ROUTE_POLICY: RoutePolicy = RoutePolicy::EnforceDoors;

/// How many recent non-admit tuples `/health` carries.
const RECENT_CAP: usize = 20;
/// How long a settings-file read of [`SETTINGS_FIELD`] is reused.
const SETTINGS_TTL: Duration = Duration::from_secs(2);

/// Origins admitted as [`OriginClass::Trusted`] with no configuration: the
/// qontinui-web dev frontend and the dev supervisor dashboard (plan F4).
pub const DEFAULT_TRUSTED_ORIGINS: &[&str] = &[
    "http://localhost:3001",
    "http://127.0.0.1:3001",
    "http://localhost:9875",
    "http://127.0.0.1:9875",
];

/// Routes no browser origin other than the runner's own webview may reach,
/// whatever the route policy.
///
/// Grammar: `"[METHOD ]pattern"`. Patterns are `MatchedPath`-form; a trailing
/// `/*` matches one or more further segments and a `{placeholder}` matches a
/// placeholder segment of the route pattern (never a literal one, so
/// `GET /state-explorer/{run_id}` does not cover `GET /state-explorer/history`). An entry with no method covers every method (for a preflight, the
/// method it names).
///
/// The first block is the plan's vetted floor. The rest came from the Phase 0
/// route census (every handler reachable from `registered_routes()` read for:
/// returns a secret, reads or writes a caller-chosen path, spawns/drives a
/// process, PTY, AI session, git operation or the runner webview, changes
/// credentials, or reaches loopback server-side and so launders a browser
/// request into a NonBrowser one), limited to routes NO Trusted or Foreign
/// caller in the census uses. App features the web dev frontend legitimately
/// drives (run a workflow, a check, a hook test) are not doors: they are on
/// [`TRUSTED_ROUTES`] only, so Foreign never reaches them where the route
/// policy enforces for Foreign (the default).
///
/// Under `enforce` the allowlists make this list redundant at runtime; it stays
/// as the Phase 1 control and as the disjointness tripwire in the tests.
pub const CREDENTIAL_DOORS: &[&str] = &[
    // --- vetted floor (plan) ---
    "/ui-bridge/invoke/*",
    "/ui-bridge/tauri/invoke",
    "/coord-mcp",
    "/coord-mcp/*",
    "/settings/ai/api-key",
    "/settings/ai/api-key/*",
    "/settings/backup/*",
    "/wrappers/{id}/credentials",
    "/wrappers/{id}/credentials/*",
    "/files/*",
    "/ui-bridge/integration/read-file",
    "/execute-python",
    "/sessions/spawn",
    "/sessions/{id}/message",
    "/terminals",
    "/terminals/*",
    // `debug_assertions` builds only — which is every supervisor-built runner,
    // so this is not a developer-laptop-only route. It drives a process in the
    // sense this list means: it types `/exit` into a live agent's pane and
    // then closes it. The `/terminals/*` entry above does NOT cover it (the
    // first segment is `__debug`), so without its own entry
    // `is_credential_door` is false for it and, under the default
    // `EnforceDoors`, a Foreign origin reaches it while the identically
    // capable `POST /terminals/{id}/write` is refused.
    "POST /__debug/terminals/{id}/graceful-exit",
    "/shell-commands/*",
    "/processes/*",
    "/instances/spawn",
    "/graphql",
    "/graphql/ws",
    "/restart-runner",
    "/drain",
    "/executor/restart",
    "/install-effects/run",
    "/ui-bridge/control/page/*",
    // --- Phase 0 route census ---
    "GET /backup",
    "POST /restore",
    "/tunnel/*",
    "/triggers",
    "/triggers/*",
    "GET /task-runs/{id}/api-requests",
    "/ui-bridge/devices",
    "/ui-bridge/devices/*",
    "POST /api/v1/streaming/pair",
    "GET /api/v1/streaming/screenshot",
    "GET /ui-bridge/control/network-request/{id}",
    "GET /ui-bridge/control/network-requests",
    "/ui-bridge/control/network-requests/*",
    "/ui-bridge/control/clipboard",
    "/ui-bridge/control/clipboard/*",
    "/ui-bridge/sdk/clipboard",
    "/ui-bridge/sdk/control/clipboard",
    "GET /ui-bridge/control/element/{id}/react-state",
    "GET /sessions/{id}/transcript",
    "/session-repository",
    "/session-repository/*",
    "GET /health/diagnostic-screenshot",
    "GET /vga/capture",
    "POST /gui-config/capture-elements",
    "/install-effects/registry-credentials/*",
    "/ui-bridge/test/coord-mcp/seed-agent-token",
    "/settings/self-healing/api-key",
    "/settings/self-healing/api-key/*",
    "DELETE /settings/playwright/password",
    "/ui-bridge/integration/*",
    "POST /configs/{id}/export",
    "POST /subagent/analyze",
    "/constraints/config",
    "/a11y/*",
    "POST /execute-steps",
    "POST /unified-workflows/execute-inline",
    "POST /ui-bridge/headless/*",
    "POST /ui-bridge/control/*",
    "POST /ui-bridge/ai/*",
    "POST /ui-bridge/batch",
    "POST /ui-bridge/ipc-response",
    "POST /ui-bridge/sdk/discover-and-cache",
    "POST /navigate",
    "POST /steward/{kind}/start",
    "POST /prompts/run",
    "POST /task-runs/session",
    "POST /coordinator/act",
    "POST /coordinator/dispatch-action",
    "POST /orchestration-loop/*",
    "POST /worktrees/*",
    "POST /session-recap/analyze",
    "POST /sessions/{id}/rewind",
    "POST /sessions/{id}/promote-to-worktree",
    "POST /sessions/{id}/commit-progress",
    "POST /instances/{id}/launch",
    "POST /instances/{id}/stop",
    "POST /launch-debug-chrome",
    "POST /bridges",
    "POST /bridges/{id}/workflow",
    "POST /vcs/pull-requests",
    "POST /agent-worktrees/reclaim",
    "POST /ui/recover",
    "DELETE /security/audit/cleanup",
    "POST /ui-bridge/relay/dispatch",
    "POST /ui-bridge/apps/{id}/dispatch",
    "POST /ui-bridge/apps/forward-device",
    "POST /ui-bridge/ios/forward",
    "POST /system/windows/activate",
    "POST /control/session-open",
    "POST /install-effects/observe-verify",
    "POST /code-semantics/typecheck",
    "GET /sse/events",
    "/ui-bridge/test/*",
    "/ui-bridge/sdk/terminal/*",
    "GET /ui-bridge/control/terminal-sessions",
    "GET /ui-bridge/control/terminal-sessions/*",
    "POST /ui-bridge/sdk/control/*",
    "POST /ui-bridge/sdk/ai/*",
    "POST /ui-bridge/sdk/execute-action-plan",
    "POST /ui-bridge/sdk/workflow/*",
    "POST /ui-bridge/sdk/transition/*",
    "POST /ui-bridge/sdk/page/read-value",
    "POST /wrappers/*",
    "DELETE /wrappers/*",
    "POST /apps/{app_id}/spec/author",
    "POST /apps/{app_id}/spec/proposals/{id}/execute",
    "POST /reflection/trigger/{task_run_id}",
    "POST /execute-action",
    "POST /gui-config/start-executor",
    "POST /state-machine/execute-transition",
    "POST /scheduler/tasks/{id}/run",
    "POST /restate/*",
    "POST /git-supervision/test-emit",
    "POST /shell-commands",
    "GET /worktrees",
    "GET /hooks/{id}",
    // a browser origin must never be able to widen browser trust
    "/settings/api/allowed-origins",
    // server-side request to a caller-chosen URL whose response is returned:
    // aimed at 127.0.0.1 it launders a browser request into a NonBrowser one
    "POST /api-request/test",
    "GET /processes",
    // --- second review: laundering / caller-named paths / hook execution ---
    // server-side requests to a caller-chosen URL, response returned
    "POST /awas/execute",
    "POST /awas/discover",
    "POST /awas/check-support",
    "POST /skills/sync/push",
    "POST /skills/sync/pull",
    "POST /evaluation/workflow",
    "POST /ui-bridge/specs/verify-api",
    "POST /extraction/start",
    "POST /uitars-extraction/start",
    "POST /ui-bridge/explore",
    "POST /knowledge/fetch-page",
    // reads (or writes under) a caller-named path
    "POST /contexts/{scope}/from-file",
    "POST /configs",
    "POST /configs/parse",
    "POST /load-config",
    "POST /vision-extraction/extract",
    "POST /extraction/vision",
    "POST /pattern/find",
    "POST /pattern/find-all",
    "GET /extraction/{extraction_id}/screenshot/{screenshot_id}",
    "POST /checks/scan-workspace",
    "POST /rag/import",
    "POST /rag/{project_id}/load",
    "POST /capture-screenshot",
    "GET /apps/{app_id}/spec/get",
    "POST /sessions/{id}/continuation-verdict",
    "GET /state-explorer/{run_id}",
    "GET /state-explorer/{run_id}/prompt",
    "POST /code-graph/*",
    "POST /code-semantics/*",
    "POST /development-intelligence/feature-health",
    "POST /file-registry/probe-conflicts",
    "GET /constraints/active",
    "GET /debug/app/errors",
    // hooks: stored commands run by `sh -c`; GET returns webhook headers/env
    "/hooks",
    "/hooks/*",
];

/// Doors the qontinui-web dev frontend (`http://localhost:3001`) calls today,
/// per the Phase 0 caller census. Under [`RoutePolicy::EnforceDoors`] (the
/// default) a request whose `Origin` is EXACTLY one of the four built-in
/// [`DEFAULT_TRUSTED_ORIGINS`] (`http://localhost:3001`,
/// `http://127.0.0.1:3001`, `http://localhost:9875`, `http://127.0.0.1:9875`)
/// is admitted, logged once per origin+route and counted as `graceAdmitted`
/// on `/health`, until qontinui-web #1380 deploys and this list is removed.
/// Operator-added Trusted origins (env or settings), every other policy and
/// every other class are refused like any door. Every entry must be a door (tripwire) and a registered route.
pub const TRUSTED_DOOR_GRACE: &[(&str, &str)] = &[
    ("POST", "/settings/ai/api-key"),
    ("DELETE", "/settings/ai/api-key/{provider}"),
    ("POST", "/settings/self-healing/api-key"),
    ("DELETE", "/settings/self-healing/api-key/{provider}"),
    ("GET", "/settings/backup/summary"),
    ("POST", "/settings/backup/export"),
    ("POST", "/settings/backup/import"),
    ("GET", "/shell-commands/{id}"),
    ("POST", "/shell-commands/{id}/run"),
    ("POST", "/awas/discover"),
    ("POST", "/awas/check-support"),
    ("POST", "/awas/execute"),
    ("POST", "/contexts/{scope}/from-file"),
    ("POST", "/configs/parse"),
    ("POST", "/load-config"),
    ("POST", "/vision-extraction/extract"),
    ("POST", "/extraction/vision"),
    ("POST", "/pattern/find"),
    ("POST", "/pattern/find-all"),
    (
        "GET",
        "/extraction/{extraction_id}/screenshot/{screenshot_id}",
    ),
    ("POST", "/checks/scan-workspace"),
    ("POST", "/rag/import"),
    ("POST", "/rag/{project_id}/load"),
    ("POST", "/capture-screenshot"),
    ("POST", "/extraction/start"),
    ("POST", "/uitars-extraction/start"),
    ("POST", "/ui-bridge/explore"),
    ("GET", "/hooks"),
    ("POST", "/hooks"),
    ("PUT", "/hooks/{id}"),
    ("DELETE", "/hooks/{id}"),
    ("PUT", "/hooks/{id}/enabled"),
    ("POST", "/hooks/{id}/test"),
];

/// Routes reachable only from the webview (FirstParty), non-browser callers
/// and [`OriginClass::Extension`] — never from a web page origin (Foreign or
/// Trusted), in EVERY route policy. `GET /ui-bridge/control/elements` is the
/// runner webview's full element inventory (rendered values included); the
/// ui-bridge extension's side panel runs as `chrome-extension://…` and keeps
/// it, while its content-script fallback (which carries the page's origin)
/// loses it.
pub const EXTENSION_ONLY_ROUTES: &[(&str, &str)] = &[("GET", "/ui-bridge/control/elements")];

/// `(METHOD, MatchedPath pattern)` pairs reachable from ANY browser origin
/// (Foreign, Extension and Trusted) under `enforce`. Derived from the Phase 0 caller
/// census: the ui-bridge extension's content scripts (which send the visited
/// page's origin, `https://*` included) and `useCommandRelay` pages.
pub const FOREIGN_ROUTES: &[(&str, &str)] = &[
    ("GET", "/health"),
    ("GET", "/livez"),
    // useCommandRelay phone-home (packages/ui-bridge/src/react/useCommandRelay.ts)
    ("POST", "/ui-bridge/apps/register"),
    ("DELETE", "/ui-bridge/apps/register/{app_id}"),
    // live relay / wrapper tabs (ws_relay.rs)
    ("GET", "/ui-bridge/ws"),
    // extension content script, bridge-client.ts
    ("GET", "/ui-bridge/annotations"),
    ("GET", "/ui-bridge/annotations/coverage"),
    ("GET", "/ui-bridge/annotations/export"),
    ("POST", "/ui-bridge/annotations/import"),
    ("GET", "/ui-bridge/annotations/{id}"),
    ("PUT", "/ui-bridge/annotations/{id}"),
    ("DELETE", "/ui-bridge/annotations/{id}"),
    // injected relay client pointed at the runner (relay-client.ts)
    ("GET", "/ui-bridge/commands/stream"),
    ("POST", "/ui-bridge/commands"),
    ("POST", "/ui-bridge/heartbeat"),
];

/// `(METHOD, MatchedPath pattern)` pairs reachable from [`OriginClass::Trusted`]
/// origins (in addition to [`FOREIGN_ROUTES`]) under `enforce`. Derived from
/// the Phase 0 caller census of qontinui-web `frontend/src/**` and
/// qontinui-supervisor `frontend/src/**`. A qontinui-web developer who adds a
/// runner call adds its route here, or gets a typed 403 naming this list.
pub const TRUSTED_ROUTES: &[(&str, &str)] = &[
    ("POST", "/ai/generate-api-request"),
    ("POST", "/ai/generate-context"),
    ("POST", "/ai/generate-macro"),
    ("POST", "/ai/generate-prompt"),
    ("POST", "/ai/generate-shell-command"),
    ("POST", "/ai/generate-test"),
    ("POST", "/ai/suggest-exploration-strategy"),
    ("POST", "/api-request/import-curl"),
    ("GET", "/apps/{app_id}/spec/list"),
    ("GET", "/awas/actions"),
    ("GET", "/check-groups"),
    ("GET", "/check-groups/{id}"),
    ("POST", "/check-groups/{id}/run"),
    ("GET", "/checks"),
    ("POST", "/checks/generate"),
    ("GET", "/checks/{id}"),
    ("POST", "/checks/{id}/run"),
    ("GET", "/contexts"),
    ("GET", "/current-execution/batch"),
    ("GET", "/disk/reclaimable"),
    ("GET", "/error-monitor/errors"),
    ("POST", "/error-monitor/errors/{id}/acknowledge"),
    ("POST", "/error-monitor/errors/{id}/resolve"),
    ("POST", "/error-monitor/fix-workflow"),
    ("POST", "/execute"),
    ("GET", "/extraction/status"),
    ("POST", "/extraction/stop"),
    ("GET", "/findings/summary"),
    ("POST", "/findings/task/{task_run_id}/clear-all"),
    ("POST", "/findings/{finding_id}/resolve"),
    ("PUT", "/findings/{finding_id}/status"),
    ("POST", "/findings/{finding_id}/user-response"),
    ("GET", "/inngest/circuit-breaker"),
    ("GET", "/inngest/events"),
    ("GET", "/inngest/queue"),
    ("GET", "/inngest/subscriptions"),
    ("POST", "/interaction-recording/start"),
    ("GET", "/interaction-recording/status"),
    ("POST", "/interaction-recording/stop"),
    ("POST", "/log-sources/migrate"),
    ("GET", "/log-sources/settings"),
    ("PUT", "/log-sources/settings"),
    ("GET", "/macros"),
    ("POST", "/macros"),
    ("DELETE", "/macros/{id}"),
    ("PUT", "/macros/{id}"),
    ("POST", "/macros/{id}/run"),
    ("GET", "/mcp-servers"),
    ("GET", "/models"),
    ("POST", "/models/delete"),
    ("GET", "/models/disk-usage"),
    ("POST", "/models/download"),
    ("GET", "/monitors"),
    ("GET", "/observations/snapshot"),
    ("GET", "/observations/stats"),
    ("GET", "/observations/temporal-search"),
    ("GET", "/observations/trends"),
    ("GET", "/observations/{id}/history"),
    ("GET", "/playwright-collection/results"),
    ("POST", "/playwright-collection/start"),
    ("GET", "/playwright-collection/status"),
    ("POST", "/playwright-collection/stop"),
    ("GET", "/playwright/tests"),
    ("POST", "/playwright/tests"),
    ("DELETE", "/playwright/tests/{id}"),
    ("PUT", "/playwright/tests/{id}"),
    ("POST", "/playwright/tests/{id}/duplicate"),
    ("POST", "/playwright/tests/{id}/run"),
    ("GET", "/prompt-snippets"),
    ("POST", "/prompt-snippets"),
    ("DELETE", "/prompt-snippets/{id}"),
    ("PUT", "/prompt-snippets/{id}"),
    ("GET", "/prompts"),
    ("POST", "/prompts"),
    ("DELETE", "/prompts/{id}"),
    ("PUT", "/prompts/{id}"),
    ("POST", "/prompts/{id}/duplicate"),
    ("GET", "/provider-health"),
    ("POST", "/provider-health/{provider_key}/reset"),
    ("GET", "/rag/availability"),
    ("GET", "/rag/list"),
    ("POST", "/rag/segment"),
    ("DELETE", "/rag/{project_id}"),
    ("GET", "/rag/{project_id}/status"),
    ("POST", "/run-workflow"),
    ("GET", "/saved-api-requests"),
    ("POST", "/saved-api-requests"),
    ("DELETE", "/saved-api-requests/{id}"),
    ("PUT", "/saved-api-requests/{id}"),
    ("POST", "/saved-api-requests/{id}/duplicate"),
    ("POST", "/scheduler/tasks"),
    ("DELETE", "/scheduler/tasks/{id}"),
    ("PUT", "/scheduler/tasks/{id}"),
    ("GET", "/settings/agentic"),
    ("PUT", "/settings/agentic"),
    ("GET", "/settings/ai"),
    ("PUT", "/settings/ai"),
    ("GET", "/settings/ai/has-key/{provider}"),
    ("POST", "/settings/ai/test-connection"),
    ("GET", "/settings/debug"),
    ("PUT", "/settings/debug"),
    ("GET", "/settings/device-info"),
    ("GET", "/settings/general"),
    ("PUT", "/settings/general"),
    ("GET", "/settings/mobile"),
    ("PUT", "/settings/mobile"),
    ("GET", "/settings/playwright"),
    ("PUT", "/settings/playwright"),
    ("GET", "/settings/self-healing"),
    ("PUT", "/settings/self-healing"),
    ("GET", "/settings/self-healing/has-key/{provider}"),
    ("GET", "/settings/storage"),
    ("POST", "/settings/storage/cleanup"),
    ("POST", "/settings/storage/clear-all"),
    ("GET", "/shell-commands"),
    ("GET", "/skills"),
    ("GET", "/state-explorer/history"),
    ("POST", "/state-explorer/start"),
    ("POST", "/state-machine/load"),
    ("GET", "/status"),
    ("GET", "/task-runs"),
    ("GET", "/task-runs/running"),
    ("DELETE", "/task-runs/{id}"),
    ("GET", "/task-runs/{id}"),
    ("PUT", "/task-runs/{id}/auto-continue"),
    ("GET", "/task-runs/{id}/checkpoints"),
    ("GET", "/task-runs/{id}/deferred-questions"),
    ("POST", "/task-runs/{id}/deferred-questions/bulk-review"),
    ("POST", "/task-runs/{id}/deferred-questions/{qid}/review"),
    ("GET", "/task-runs/{id}/events"),
    ("POST", "/task-runs/{id}/generate-summary"),
    ("GET", "/task-runs/{id}/knowledge"),
    ("GET", "/task-runs/{id}/mcp-calls"),
    ("POST", "/task-runs/{id}/message"),
    ("GET", "/task-runs/{id}/orchestrator-state"),
    ("GET", "/task-runs/{id}/output"),
    ("POST", "/task-runs/{id}/pause"),
    ("GET", "/task-runs/{id}/playwright-results"),
    ("GET", "/task-runs/{id}/result-data"),
    ("POST", "/task-runs/{id}/resume"),
    ("GET", "/task-runs/{id}/screenshots"),
    ("GET", "/task-runs/{id}/session-state"),
    ("POST", "/task-runs/{id}/stop"),
    ("GET", "/task-runs/{id}/verification-phase-results"),
    ("GET", "/task-runs/{id}/verification-results"),
    ("GET", "/task-runs/{id}/workflow-state"),
    ("GET", "/test-results"),
    ("GET", "/testing/active-states"),
    ("POST", "/testing/assertion"),
    ("POST", "/testing/end/{id}"),
    ("POST", "/testing/mock-mode"),
    ("GET", "/testing/results/{id}"),
    ("GET", "/testing/runs"),
    ("POST", "/testing/start"),
    ("GET", "/testing/states"),
    ("POST", "/testing/traverse"),
    ("GET", "/tests"),
    ("POST", "/tests"),
    ("DELETE", "/tests/{id}"),
    ("GET", "/tests/{id}"),
    ("PUT", "/tests/{id}"),
    ("POST", "/tests/{id}/execute"),
    ("PUT", "/ui-bridge/annotations/{id}"),
    ("POST", "/ui-bridge/apps/scan"),
    ("GET", "/ui-bridge/apps/scan/desktop"),
    ("GET", "/ui-bridge/apps/scan/web"),
    ("GET", "/ui-bridge/control/snapshot"),
    ("GET", "/ui-bridge/explore/results"),
    ("GET", "/ui-bridge/explore/status"),
    ("POST", "/ui-bridge/explore/stop"),
    ("POST", "/ui-bridge/sdk/connect"),
    ("GET", "/ui-bridge/sdk/connections"),
    ("POST", "/ui-bridge/sdk/debug/highlight/{id}"),
    ("POST", "/ui-bridge/sdk/disconnect"),
    ("POST", "/ui-bridge/sdk/discover"),
    ("POST", "/ui-bridge/sdk/element/{id}/action"),
    ("POST", "/ui-bridge/sdk/page/navigate"),
    ("GET", "/ui-bridge/sdk/snapshot"),
    ("POST", "/ui-bridge/sdk/switch"),
    ("GET", "/ui-bridge/sdk/tabs"),
    ("GET", "/uitars-extraction/status"),
    ("POST", "/uitars-extraction/stop"),
    ("GET", "/unified-workflows"),
    ("POST", "/unified-workflows"),
    ("POST", "/unified-workflows/generate-async"),
    ("POST", "/unified-workflows/run-composed"),
    ("POST", "/unified-workflows/{id}/run"),
    ("GET", "/ws/events"),
];

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Who a request is, as far as its headers can say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginClass {
    /// No `Origin`, and no cross-site Fetch Metadata: agents, scripts, curl.
    NonBrowser,
    /// The runner's own webview, or a page the runner itself serves.
    FirstParty,
    /// A configured browser origin (defaults + env + settings).
    Trusted,
    /// A browser extension's own page (`chrome-extension://`, `moz-extension://`,
    /// `safari-web-extension://`) — the ui-bridge side panel. NOT an extension
    /// content script, which carries the visited page's origin.
    Extension,
    /// Any other browser origin, including `null` and a cross-site no-cors
    /// request that carries no `Origin` at all.
    Foreign,
}

impl OriginClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NonBrowser => "non_browser",
            Self::FirstParty => "first_party",
            Self::Trusted => "trusted",
            Self::Extension => "extension",
            Self::Foreign => "foreign",
        }
    }
}

/// Phase 2 mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutePolicy {
    /// Both browser classes are held to their allowlists.
    Enforce,
    /// Foreign and Extension are held to their allowlists; Trusted is shadowed.
    EnforceForeign,
    /// Doors enforced (Trusted [`TRUSTED_DOOR_GRACE`] logged, not refused);
    /// every allowlist shadowed. The default.
    EnforceDoors,
    /// Doors enforced with no grace; allowlists shadowed (logged + counted).
    Shadow,
    /// Doors enforced with no grace; no allowlist metering (Phase 1; never
    /// CORS `*`).
    Off,
}

impl RoutePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enforce => "enforce",
            Self::EnforceForeign => "enforce-foreign",
            Self::EnforceDoors => "enforce-doors",
            Self::Shadow => "shadow",
            Self::Off => "off",
        }
    }

    /// Parse the env value. Unset → [`DEFAULT_ROUTE_POLICY`]; an
    /// unrecognised value also falls back to the default (and says so).
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(|s| s.trim().to_ascii_lowercase()) {
            None => DEFAULT_ROUTE_POLICY,
            Some(v) if v.is_empty() => DEFAULT_ROUTE_POLICY,
            Some(v) if v == "enforce" => Self::Enforce,
            Some(v) if v == "enforce-foreign" || v == "enforce_foreign" => Self::EnforceForeign,
            Some(v) if v == "enforce-doors" || v == "enforce_doors" => Self::EnforceDoors,
            Some(v) if v == "shadow" => Self::Shadow,
            Some(v) if v == "off" => Self::Off,
            Some(v) => {
                tracing::warn!(
                    value = %v,
                    default = DEFAULT_ROUTE_POLICY.as_str(),
                    "{ENV_ROUTE_POLICY}: unrecognised value, using the default"
                );
                DEFAULT_ROUTE_POLICY
            }
        }
    }
}

/// Inserted into every admitted request's extensions, so a handler (`/health`)
/// can tailor what it reveals to the class of its caller.
#[derive(Debug, Clone, Copy)]
pub struct RequesterClass(pub OriginClass);

/// What the CORS layer may grant this request. Set by the guard; the CORS
/// layer sits inside it and reads this instead of re-classifying.
#[derive(Debug, Clone, Copy)]
struct CorsGrant {
    allow_origin: bool,
    private_network: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Admit,
    /// Admitted only because the route policy shadows this class.
    ShadowWouldRefuse,
    /// A [`TRUSTED_DOOR_GRACE`] door admitted for a built-in default Trusted
    /// origin under `enforce-doors`. Distinct from a shadow: this is a DOOR.
    GraceAdmitted,
    RefuseHost,
    /// A [`CREDENTIAL_DOORS`] route from a non-first-party browser origin.
    RefuseDoor,
    /// An [`EXTENSION_ONLY_ROUTES`] route from a web page origin.
    RefuseExtensionOnly,
    /// Not on the class's allowlist, and the policy enforces for the class.
    RefuseRoute,
}

impl Verdict {
    fn as_str(self) -> &'static str {
        match self {
            Self::Admit => "admit",
            Self::ShadowWouldRefuse => "shadow_would_refuse",
            Self::GraceAdmitted => "grace_admitted",
            Self::RefuseHost => "refused_host",
            Self::RefuseDoor => "refused_credential_door",
            Self::RefuseExtensionOnly => "refused_extension_only",
            Self::RefuseRoute => "refused_route_policy",
        }
    }
}

type PortSource = Arc<dyn Fn() -> u16 + Send + Sync>;
type OriginsSource = Arc<dyn Fn() -> Arc<Vec<NormOrigin>> + Send + Sync>;

#[derive(Default)]
struct Stats {
    refused_host: AtomicU64,
    refused_trusted: AtomicU64,
    refused_foreign: AtomicU64,
    refused_extension: AtomicU64,
    shadow_trusted: AtomicU64,
    grace_trusted: AtomicU64,
    shadow_foreign: AtomicU64,
    shadow_extension: AtomicU64,
    recent: Mutex<VecDeque<Value>>,
}

/// The guard's configuration and counters. One per router.
pub struct OriginGuard {
    enabled: bool,
    route_policy: RoutePolicy,
    bound_port: PortSource,
    env_origins: Vec<NormOrigin>,
    env_hosts: Vec<String>,
    settings_origins: OriginsSource,
    stats: Stats,
}

/// The process's installed guard, for `/health`.
static INSTALLED: OnceLock<Arc<OriginGuard>> = OnceLock::new();

impl OriginGuard {
    /// Build from raw config values. `guard_env` / `policy_env` /
    /// `origins_env` / `hosts_env` are the values of [`ENV_GUARD`],
    /// [`ENV_ROUTE_POLICY`], [`ENV_ALLOWED_ORIGINS`], [`ENV_ALLOWED_HOSTS`].
    pub fn new(
        guard_env: Option<&str>,
        policy_env: Option<&str>,
        origins_env: Option<&str>,
        hosts_env: Option<&str>,
        bound_port: PortSource,
        settings_origins: OriginsSource,
    ) -> Self {
        let enabled = guard_env.map(|v| v.trim() != "0").unwrap_or(true);
        Self {
            enabled,
            route_policy: RoutePolicy::parse(policy_env),
            bound_port,
            env_origins: split_list(origins_env)
                .iter()
                .filter_map(|o| parse_configured_origin(ENV_ALLOWED_ORIGINS, o))
                .collect(),
            env_hosts: split_list(hosts_env)
                .into_iter()
                .map(|h| h.to_ascii_lowercase())
                .collect(),
            settings_origins,
            stats: Stats::default(),
        }
    }

    /// The production guard: env read now (once, at spawn), the bound port
    /// read per request from `AppState.api_port`, and [`SETTINGS_FIELD`]
    /// refreshed from the settings file OFF the request path (primed here, then
    /// a background re-read at most every [`SETTINGS_TTL`]) so an operator can
    /// admit a dev origin without a restart.
    pub fn from_env(app_state: Arc<crate::commands::AppState>) -> Self {
        prime_settings_origins();
        let env = |k: &str| std::env::var(k).ok();
        Self::new(
            env(ENV_GUARD).as_deref(),
            env(ENV_ROUTE_POLICY).as_deref(),
            env(ENV_ALLOWED_ORIGINS).as_deref(),
            env(ENV_ALLOWED_HOSTS).as_deref(),
            Arc::new(move || app_state.api_port.load(Ordering::Relaxed)),
            Arc::new(settings_allowed_origins),
        )
    }

    /// Record this guard as the process's installed one (for `/health`).
    /// First install wins; a second router build keeps the first.
    pub fn install(self: &Arc<Self>) {
        let _ = INSTALLED.set(self.clone());
        tracing::info!(
            enabled = self.enabled,
            route_policy = self.route_policy.as_str(),
            extra_env_origins = self.env_origins.len(),
            extra_env_hosts = self.env_hosts.len(),
            "origin guard installed"
        );
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn route_policy(&self) -> RoutePolicy {
        self.route_policy
    }

    // -- classification --------------------------------------------------

    fn host_admitted(&self, headers: &HeaderMap) -> bool {
        let Some(raw) = headers.get(header::HOST) else {
            return true;
        };
        let Ok(host) = raw.to_str() else {
            return false;
        };
        let host = host.trim().to_ascii_lowercase();
        if self.env_hosts.contains(&host) {
            return true;
        }
        let (name, port) = if let Some(rest) = host.strip_prefix('[') {
            match rest.split_once(']') {
                Some((inner, tail)) => (format!("[{inner}]"), tail.strip_prefix(':')),
                None => return false,
            }
        } else {
            match host.rsplit_once(':') {
                Some((n, p)) => (n.to_string(), Some(p)),
                None => (host.clone(), None),
            }
        };
        if tunnel_host_admitted(&name) {
            return true;
        }
        // 10.0.2.2 / 10.0.3.2 are the Android emulator's (AVD / Genymotion)
        // aliases for the host's loopback. An IP-literal Host cannot be
        // produced by DNS rebinding, which needs a hostname.
        let loopback = matches!(
            name.as_str(),
            "127.0.0.1" | "localhost" | "[::1]" | "10.0.2.2" | "10.0.3.2"
        );
        let bound = (self.bound_port)();
        loopback && port.and_then(|p| p.parse::<u16>().ok()) == Some(bound)
    }

    #[cfg(test)]
    fn classify(&self, headers: &HeaderMap) -> OriginClass {
        self.classify_detail(headers).0
    }

    /// The origin class, and whether the (already-parsed) `Origin` is exactly
    /// one of the built-in [`DEFAULT_TRUSTED_ORIGINS`] — the gate for
    /// [`TRUSTED_DOOR_GRACE`]. Parsed once here, not again by the grace check.
    fn classify_detail(&self, headers: &HeaderMap) -> (OriginClass, bool) {
        let Some(origin) = headers.get(header::ORIGIN) else {
            let sfs = headers
                .get("sec-fetch-site")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.trim().to_ascii_lowercase());
            let class = match sfs.as_deref() {
                Some("cross-site") | Some("same-site") => OriginClass::Foreign,
                _ => OriginClass::NonBrowser,
            };
            return (class, false);
        };
        let Some(norm) = origin.to_str().ok().and_then(NormOrigin::parse) else {
            return (OriginClass::Foreign, false);
        };
        if self.is_first_party(&norm) {
            return (OriginClass::FirstParty, false);
        }
        if matches!(
            norm.scheme.as_str(),
            "chrome-extension" | "moz-extension" | "safari-web-extension"
        ) {
            return (OriginClass::Extension, false);
        }
        if default_trusted_origins().contains(&norm) {
            return (OriginClass::Trusted, true);
        }
        if self.env_origins.contains(&norm) || (self.settings_origins)().contains(&norm) {
            return (OriginClass::Trusted, false);
        }
        (OriginClass::Foreign, false)
    }

    fn is_first_party(&self, o: &NormOrigin) -> bool {
        match (o.scheme.as_str(), o.host.as_str()) {
            ("tauri", "localhost") => o.port.is_none(),
            ("http", "tauri.localhost") => o.port == Some(80),
            ("https", "tauri.localhost") => o.port == Some(443),
            ("http", "127.0.0.1" | "localhost" | "[::1]") => o.port == Some((self.bound_port)()),
            _ => false,
        }
    }

    /// The whole decision for one request's head.
    fn decide(&self, parts_method: &Method, headers: &HeaderMap, route: Option<&str>) -> Decision {
        let (class, default_trusted) = self.classify_detail(headers);
        let method = effective_method(parts_method, headers);
        let origin = headers
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let mk = |verdict| Decision {
            class,
            verdict,
            method: method.clone(),
            origin: origin.clone(),
            route: route.map(str::to_string),
        };
        if !self.enabled {
            return mk(Verdict::Admit);
        }
        if !self.host_admitted(headers) {
            return mk(Verdict::RefuseHost);
        }
        let Some(route) = route else {
            // Unmatched: the canonical 404 answers, whatever the class.
            return mk(Verdict::Admit);
        };
        if matches!(class, OriginClass::NonBrowser | OriginClass::FirstParty) {
            return mk(Verdict::Admit);
        }
        if listed(EXTENSION_ONLY_ROUTES, &method, route) {
            return if class == OriginClass::Extension {
                mk(Verdict::Admit)
            } else {
                mk(Verdict::RefuseExtensionOnly)
            };
        }
        if is_credential_door(&method, route) {
            if class == OriginClass::Trusted
                && self.route_policy == RoutePolicy::EnforceDoors
                && listed(TRUSTED_DOOR_GRACE, &method, route)
                && default_trusted
            {
                return mk(Verdict::GraceAdmitted);
            }
            return mk(Verdict::RefuseDoor);
        }
        if self.route_policy == RoutePolicy::Off {
            return mk(Verdict::Admit);
        }
        if route_allowed(class, &method, route) {
            return mk(Verdict::Admit);
        }
        let enforced = match self.route_policy {
            RoutePolicy::Enforce => true,
            RoutePolicy::EnforceForeign => class != OriginClass::Trusted,
            RoutePolicy::EnforceDoors | RoutePolicy::Shadow | RoutePolicy::Off => false,
        };
        if enforced {
            mk(Verdict::RefuseRoute)
        } else {
            mk(Verdict::ShadowWouldRefuse)
        }
    }

    fn record(&self, d: &Decision) {
        let s = &self.stats;
        match (d.verdict, d.class) {
            (Verdict::Admit, _) => return,
            (Verdict::RefuseHost, _) => s.refused_host.fetch_add(1, Ordering::Relaxed),
            (Verdict::GraceAdmitted, _) => s.grace_trusted.fetch_add(1, Ordering::Relaxed),
            (Verdict::ShadowWouldRefuse, OriginClass::Trusted) => {
                s.shadow_trusted.fetch_add(1, Ordering::Relaxed)
            }
            (Verdict::ShadowWouldRefuse, OriginClass::Extension) => {
                s.shadow_extension.fetch_add(1, Ordering::Relaxed)
            }
            (Verdict::ShadowWouldRefuse, _) => s.shadow_foreign.fetch_add(1, Ordering::Relaxed),
            (_, OriginClass::Trusted) => s.refused_trusted.fetch_add(1, Ordering::Relaxed),
            (_, OriginClass::Extension) => s.refused_extension.fetch_add(1, Ordering::Relaxed),
            (_, _) => s.refused_foreign.fetch_add(1, Ordering::Relaxed),
        };
        if let Ok(mut recent) = s.recent.lock() {
            if recent.len() == RECENT_CAP {
                recent.pop_front();
            }
            recent.push_back(json!({
                "origin": d.origin,
                "class": d.class.as_str(),
                "method": d.method,
                "routePattern": d.route,
                "verdict": d.verdict.as_str(),
            }));
        }
    }

    /// The `/health` `originGuard` block. The recent tuples name other sites
    /// the operator's browser pointed at this runner, so they are shown only
    /// to the webview and non-browser callers (`None` = a direct in-process
    /// read) and withheld from every browser class — `/health` is on
    /// [`FOREIGN_ROUTES`].
    pub fn health_json(&self, requester: Option<OriginClass>) -> Value {
        let s = &self.stats;
        let load = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let mut out = json!({
            "installed": true,
            "enabled": self.enabled,
            "routePolicy": self.route_policy.as_str(),
            "refusals": {
                "host": load(&s.refused_host),
                "trusted": load(&s.refused_trusted),
                "foreign": load(&s.refused_foreign),
                "extension": load(&s.refused_extension),
                // Never refused by design; present so a reader sees an explicit 0.
                "first_party": 0,
            },
            "shadowWouldRefuse": {
                "trusted": load(&s.shadow_trusted),
                "foreign": load(&s.shadow_foreign),
                "extension": load(&s.shadow_extension),
            },
            // Doors admitted under TRUSTED_DOOR_GRACE (default dev origins only).
            "graceAdmitted": {
                "trusted": load(&s.grace_trusted),
            },
            "trustedDoorGrace": TRUSTED_DOOR_GRACE.len(),
            "admitOriginEnv": ENV_ALLOWED_ORIGINS,
            "admitOriginSetting": SETTINGS_FIELD,
            "killSwitchEnv": ENV_GUARD,
            "routePolicyEnv": ENV_ROUTE_POLICY,
        });
        if matches!(
            requester,
            None | Some(OriginClass::NonBrowser) | Some(OriginClass::FirstParty)
        ) {
            let recent: Vec<Value> = s
                .recent
                .lock()
                .map(|r| r.iter().cloned().collect())
                .unwrap_or_default();
            out["recent"] = Value::Array(recent);
        }
        out
    }
}

struct Decision {
    class: OriginClass,
    verdict: Verdict,
    method: String,
    origin: Option<String>,
    route: Option<String>,
}

/// The `/health` block for the installed guard, or `installed: false`.
pub fn health_json(requester: Option<OriginClass>) -> Value {
    match INSTALLED.get() {
        Some(g) => g.health_json(requester),
        None => json!({ "installed": false }),
    }
}

// ---------------------------------------------------------------------------
// Middleware + CORS
// ---------------------------------------------------------------------------

/// Apply the guard and the CORS layer to `router`, guard OUTSIDE CORS, so a
/// refused request (preflights included) never reaches CORS or a handler.
/// The one registration `create_router` makes, and the one the tests drive.
pub fn apply<S>(router: Router<S>, guard: Arc<OriginGuard>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router
        .layer(cors_layer())
        .layer(axum::middleware::from_fn_with_state(
            guard,
            origin_guard_middleware,
        ))
}

/// CORS that echoes only what the guard granted — never `*`.
pub fn cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|_origin, parts| {
            parts
                .extensions
                .get::<CorsGrant>()
                .map(|g| g.allow_origin)
                .unwrap_or(false)
        }))
        .allow_methods(Any)
        .allow_headers(Any)
        .allow_private_network(AllowPrivateNetwork::predicate(|_origin, parts| {
            parts
                .extensions
                .get::<CorsGrant>()
                .map(|g| g.private_network)
                .unwrap_or(false)
        }))
}

async fn origin_guard_middleware(
    State(guard): State<Arc<OriginGuard>>,
    mut req: Request,
    next: Next,
) -> Response {
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string());
    let d = guard.decide(req.method(), req.headers(), route.as_deref());
    guard.record(&d);
    match d.verdict {
        Verdict::Admit | Verdict::ShadowWouldRefuse | Verdict::GraceAdmitted => {
            if d.verdict == Verdict::GraceAdmitted && first_shadow_sighting(&d) {
                tracing::warn!(
                    origin = ?d.origin,
                    method = %d.method,
                    route = ?d.route,
                    "origin guard (grace): ADMITTED a credential door for a default dev origin under TRUSTED_DOOR_GRACE (removed when qontinui-web #1380 deploys; logged once per origin+route; counted on /health graceAdmitted)"
                );
            }
            if d.verdict == Verdict::ShadowWouldRefuse && first_shadow_sighting(&d) {
                tracing::warn!(
                    origin = ?d.origin,
                    class = d.class.as_str(),
                    method = %d.method,
                    route = ?d.route,
                    "origin guard (shadow): would refuse where {ENV_ROUTE_POLICY} enforces this class (logged once per origin+route; counted on /health)"
                );
            }
            // Everything admitted may read its reply: the exact origin is
            // echoed, never `*`. Private-network access is granted only to
            // the webview and configured origins.
            req.extensions_mut().insert(CorsGrant {
                allow_origin: true,
                private_network: matches!(d.class, OriginClass::FirstParty | OriginClass::Trusted),
            });
            req.extensions_mut().insert(RequesterClass(d.class));
            next.run(req).await
        }
        Verdict::RefuseHost => {
            tracing::warn!(method = %d.method, route = ?d.route, "origin guard: refused a non-loopback Host");
            refusal(
                CODE_HOST_NOT_LOOPBACK,
                "Host header does not name this runner's loopback address and bound port",
                json!({
                    "host": req.headers().get(header::HOST).and_then(|v| v.to_str().ok()),
                    "admitHostEnv": ENV_ALLOWED_HOSTS,
                }),
                None,
            )
        }
        Verdict::RefuseDoor | Verdict::RefuseExtensionOnly | Verdict::RefuseRoute => {
            tracing::warn!(
                origin = ?d.origin,
                class = d.class.as_str(),
                method = %d.method,
                route = ?d.route,
                verdict = d.verdict.as_str(),
                "origin guard: refused a browser origin"
            );
            let message = match d.verdict {
                Verdict::RefuseDoor => "This route returns credentials, reads caller-named paths, makes caller-directed requests or executes code, and is not reachable from a browser origin other than the runner's own webview",
                Verdict::RefuseExtensionOnly => "This route is reachable only from the runner's webview and the ui-bridge extension's own pages, not from a web page origin",
                _ => "This route is not on the allowlist for this origin class",
            };
            let echo = (d.class == OriginClass::Trusted)
                .then(|| req.headers().get(header::ORIGIN).cloned())
                .flatten();
            refusal(
                CODE_CROSS_ORIGIN_REFUSED,
                message,
                json!({
                    "origin": d.origin,
                    "class": d.class.as_str(),
                    "method": d.method,
                    "route_pattern": d.route,
                    "reason": d.verdict.as_str(),
                    "routePolicy": guard.route_policy.as_str(),
                    "admitOriginEnv": ENV_ALLOWED_ORIGINS,
                    "admitOriginSetting": SETTINGS_FIELD,
                    "allowlists": "mcp::origin_guard::{FOREIGN_ROUTES, TRUSTED_ROUTES}",
                }),
                echo,
            )
        }
    }
}

fn refusal(
    code: &str,
    message: &str,
    context: Value,
    echo_origin: Option<HeaderValue>,
) -> Response {
    let body = json!({
        "success": false,
        "error": message,
        "code": code,
        "context": context,
    });
    let mut resp = (StatusCode::FORBIDDEN, axum::Json(body)).into_response();
    if let Some(origin) = echo_origin {
        let h = resp.headers_mut();
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        h.append(header::VARY, HeaderValue::from_static("origin"));
    }
    resp
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// A preflight is judged by the method it asks about.
fn effective_method(method: &Method, headers: &HeaderMap) -> String {
    if method == Method::OPTIONS {
        if let Some(m) = headers
            .get(header::ACCESS_CONTROL_REQUEST_METHOD)
            .and_then(|v| v.to_str().ok())
        {
            return m.trim().to_ascii_uppercase();
        }
    }
    if method == Method::HEAD {
        return "GET".to_string();
    }
    method.as_str().to_ascii_uppercase()
}

fn is_placeholder(seg: &str) -> bool {
    seg.starts_with('{') && seg.ends_with('}')
}

/// Does door entry `entry` cover `(method, route)`? See [`CREDENTIAL_DOORS`]
/// for the grammar.
pub(crate) fn door_matches(entry: &str, method: &str, route: &str) -> bool {
    let entry = match entry.split_once(' ') {
        Some((m, path)) => {
            if !m.eq_ignore_ascii_case(method) {
                return false;
            }
            path
        }
        None => entry,
    };
    let e: Vec<&str> = entry.split('/').filter(|s| !s.is_empty()).collect();
    let r: Vec<&str> = route.split('/').filter(|s| !s.is_empty()).collect();
    let (e, wildcard) = match e.split_last() {
        Some((&"*", head)) => (head.to_vec(), true),
        _ => (e, false),
    };
    if wildcard {
        if r.len() <= e.len() {
            return false;
        }
    } else if r.len() != e.len() {
        return false;
    }
    e.iter()
        .zip(r.iter())
        .all(|(a, b)| (is_placeholder(a) && is_placeholder(b)) || a.eq_ignore_ascii_case(b))
}

fn listed(list: &[(&str, &str)], method: &str, route: &str) -> bool {
    list.iter().any(|(m, p)| *m == method && *p == route)
}

pub(crate) fn is_credential_door(method: &str, route: &str) -> bool {
    CREDENTIAL_DOORS
        .iter()
        .any(|e| door_matches(e, method, route))
}

fn route_allowed(class: OriginClass, method: &str, route: &str) -> bool {
    let hit = |list: &[(&str, &str)]| list.iter().any(|(m, p)| *m == method && *p == route);
    match class {
        OriginClass::NonBrowser | OriginClass::FirstParty => true,
        OriginClass::Trusted => hit(FOREIGN_ROUTES) || hit(TRUSTED_ROUTES),
        OriginClass::Foreign | OriginClass::Extension => hit(FOREIGN_ROUTES),
    }
}

fn split_list(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// An origin compared on scheme + host + port, the way
/// `commands/web_integration.rs` compares origins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormOrigin {
    scheme: String,
    host: String,
    port: Option<u16>,
}

impl NormOrigin {
    fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim().trim_end_matches('/');
        if raw.is_empty() || raw.eq_ignore_ascii_case("null") {
            return None;
        }
        let u = url::Url::parse(raw).ok()?;
        let host = u.host_str()?.to_ascii_lowercase();
        Some(Self {
            scheme: u.scheme().to_ascii_lowercase(),
            host,
            port: u.port_or_known_default(),
        })
    }
}

/// Validate and canonicalise one operator-supplied origin for
/// [`SETTINGS_FIELD`]: `scheme://host[:port]`, no path, no wildcard, not
/// `null`. Exactly the runner webview's own origins (`tauri://localhost`,
/// `http(s)://tauri.localhost`) are refused as pointless; a loopback origin is
/// accepted, since which port is "first-party" depends on the bind.
pub fn canonical_allowed_origin(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    let webview = [
        "tauri://localhost",
        "http://tauri.localhost",
        "https://tauri.localhost",
    ];
    if let Some(n) = NormOrigin::parse(trimmed) {
        if webview
            .iter()
            .filter_map(|w| NormOrigin::parse(w))
            .any(|w| w == n)
        {
            return Err(format!(
                "{trimmed:?}: the runner webview's own origin needs no entry"
            ));
        }
    }
    let lower = trimmed.to_ascii_lowercase();
    if [
        "chrome-extension:",
        "moz-extension:",
        "safari-web-extension:",
    ]
    .iter()
    .any(|p| lower.starts_with(p))
    {
        return Err(format!(
            "{trimmed:?}: extension origins are classified as the extension class by scheme, never Trusted; an entry would have no effect"
        ));
    }
    if trimmed.contains('*') {
        return Err(format!(
            "{trimmed:?}: wildcards are not accepted; list exact origins"
        ));
    }
    let u = url::Url::parse(trimmed).map_err(|e| format!("{trimmed:?}: not a URL ({e})"))?;
    if !(u.path().is_empty() || u.path() == "/") || u.query().is_some() || u.fragment().is_some() {
        return Err(format!(
            "{trimmed:?}: an origin has no path, query or fragment"
        ));
    }
    let n = NormOrigin::parse(trimmed).ok_or_else(|| format!("{trimmed:?}: no host"))?;
    let default_port = url::Url::parse(&format!("{}://{}", n.scheme, n.host))
        .ok()
        .and_then(|d| d.port_or_known_default());
    Ok(match n.port {
        Some(p) if Some(p) != default_port => format!("{}://{}:{}", n.scheme, n.host, p),
        _ => format!("{}://{}", n.scheme, n.host),
    })
}

/// Hostname of the active rathole tunnel server, if one is running. A
/// tunnelled request is a raw TCP forward to `127.0.0.1:<port>` that keeps the
/// remote client's `Host` (the tunnel server's name), so the Host gate admits
/// that hostname, on any port, while the tunnel is up. Set by
/// `mcp::tunnel_api` on start/stop.
static TUNNEL_HOST: Mutex<Option<String>> = Mutex::new(None);

pub fn set_tunnel_server_addr(server_addr: Option<&str>) {
    let host = server_addr.map(|a| {
        let a = a.trim().to_ascii_lowercase();
        let a = a.split_once("://").map(|(_, r)| r.to_string()).unwrap_or(a);
        match a.strip_prefix('[') {
            Some(rest) => rest
                .split_once(']')
                .map(|(i, _)| format!("[{i}]"))
                .unwrap_or_else(|| a.clone()),
            None => a
                .rsplit_once(':')
                .map(|(h, _)| h.to_string())
                .unwrap_or(a.clone()),
        }
    });
    if let Ok(mut g) = TUNNEL_HOST.lock() {
        *g = host.filter(|h| !h.is_empty());
    }
}

fn tunnel_host_admitted(name: &str) -> bool {
    TUNNEL_HOST
        .lock()
        .map(|g| g.as_deref() == Some(name))
        .unwrap_or(false)
}

/// True the first time a shadow would-refuse is seen for this
/// (class, origin, method, route) — so a polling page logs one WARN, not one
/// per poll. Bounded; past the bound every sighting is "not first" (the
/// `/health` counters still count them all).
fn first_shadow_sighting(d: &Decision) -> bool {
    static SEEN: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);
    let key = format!(
        "{}|{:?}|{}|{:?}",
        d.class.as_str(),
        d.origin,
        d.method,
        d.route
    );
    let Ok(mut g) = SEEN.lock() else {
        return false;
    };
    let set = g.get_or_insert_with(Default::default);
    if set.len() >= 512 {
        return false;
    }
    set.insert(key)
}

/// Parse a configured origin, WARN-logging (once per value) one that does not
/// parse, so a typo in the env var or settings is visible rather than silently
/// never matching.
fn parse_configured_origin(source: &str, raw: &str) -> Option<NormOrigin> {
    let parsed = NormOrigin::parse(raw);
    if parsed.is_none() {
        static WARNED: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);
        let first = WARNED
            .lock()
            .map(|mut g| {
                let set = g.get_or_insert_with(Default::default);
                set.len() < 256 && set.insert(format!("{source}|{raw}"))
            })
            .unwrap_or(false);
        if first {
            tracing::warn!(source, value = %raw, "origin guard: ignoring an unparseable allowed origin");
        }
    }
    parsed
}

/// Cached, already-parsed [`SETTINGS_FIELD`] value and when it was read.
#[allow(clippy::type_complexity)]
static SETTINGS_CACHE: Mutex<Option<(Instant, Arc<Vec<NormOrigin>>)>> = Mutex::new(None);
static SETTINGS_REFRESH_IN_FLIGHT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Read and parse once per refresh (never per request); unparseable entries
/// WARN once.
fn read_settings_origins() -> Arc<Vec<NormOrigin>> {
    Arc::new(
        crate::settings::read_settings_from_disk()
            .settings
            .api
            .allowed_origins
            .iter()
            .filter_map(|o| parse_configured_origin(SETTINGS_FIELD, o))
            .collect(),
    )
}

fn default_trusted_origins() -> &'static [NormOrigin] {
    static DEFAULTS: OnceLock<Vec<NormOrigin>> = OnceLock::new();
    DEFAULTS.get_or_init(|| {
        DEFAULT_TRUSTED_ORIGINS
            .iter()
            .filter_map(|o| NormOrigin::parse(o))
            .collect()
    })
}

/// Synchronous first read, at router build (startup), so the request path
/// never blocks on disk.
fn prime_settings_origins() {
    let fresh = read_settings_origins();
    if let Ok(mut g) = SETTINGS_CACHE.lock() {
        *g = Some((Instant::now(), fresh));
    }
}

/// [`SETTINGS_FIELD`] as last read. Never touches disk on the calling thread:
/// when the cached value is older than [`SETTINGS_TTL`] it is returned as-is
/// and ONE background thread re-reads the settings file (the non-mutating,
/// mtime-cached reader), so a save is live within about the TTL.
fn settings_allowed_origins() -> Arc<Vec<NormOrigin>> {
    let (value, stale) = match SETTINGS_CACHE.lock() {
        Ok(g) => match g.as_ref() {
            Some((at, v)) => (v.clone(), at.elapsed() >= SETTINGS_TTL),
            None => (Arc::new(Vec::new()), true),
        },
        Err(_) => (Arc::new(Vec::new()), false),
    };
    if stale && !SETTINGS_REFRESH_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        let spawned = std::thread::Builder::new()
            .name("origin-guard-settings".into())
            .spawn(|| {
                let fresh = read_settings_origins();
                if let Ok(mut g) = SETTINGS_CACHE.lock() {
                    *g = Some((Instant::now(), fresh));
                }
                SETTINGS_REFRESH_IN_FLIGHT.store(false, Ordering::SeqCst);
            });
        if spawned.is_err() {
            SETTINGS_REFRESH_IN_FLIGHT.store(false, Ordering::SeqCst);
        }
    }
    value
}

/// `.route`/`.merge`/`.nest`/`.fallback` calls that `create_router`'s source
/// makes AFTER `origin_guard::apply(` — each would register a route the guard
/// does not wrap (axum applies `Router::layer` only to routes present when it
/// is called). Pure over source text so the tripwire can be proven on a
/// synthetic input.
#[cfg(test)]
pub(crate) fn registrations_after_apply(src: &str) -> Vec<String> {
    let Some(start) = src.find("pub fn create_router(") else {
        return vec!["create_router not found".to_string()];
    };
    let body = &src[start..];
    let end = body.find("\n}\n").map(|i| i + 1).unwrap_or(body.len());
    let body = &body[..end];
    let Some(apply_at) = body.find("origin_guard::apply(") else {
        return vec!["origin_guard::apply( not found in create_router".to_string()];
    };
    let tail = &body[apply_at..];
    let mut hits = Vec::new();
    for (n, line) in tail.lines().enumerate() {
        let code = line.split("//").next().unwrap_or("");
        for needle in [
            ".route(",
            ".route_service(",
            ".merge(",
            ".nest(",
            ".nest_service(",
            ".fallback(",
            ".fallback_service(",
        ] {
            if code.contains(needle) {
                hits.push(format!("+{n}: {}", line.trim()));
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests;
