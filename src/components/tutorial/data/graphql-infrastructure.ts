/**
 * GraphQL Infrastructure Tutorial
 *
 * Informational tutorial explaining the runner's full-stack GraphQL system —
 * how it works, why it exists, and how the pieces connect. Covers the backend
 * schema (async-graphql + Axum), the frontend Apollo Client, the typed hooks
 * library, real-time subscriptions, and the codegen pipeline.
 */

import type { Tutorial } from "../../../types/tutorial";

export const graphqlInfrastructureTutorial: Tutorial = {
  id: "graphql-infrastructure",
  title: "GraphQL Infrastructure Deep Dive",
  description:
    "Understand the runner's full-stack GraphQL system — from Rust resolvers to Apollo Client to real-time WebSocket subscriptions. Learn how it coexists with the REST API and replaces polling with server-pushed events.",
  duration: "15 minutes",
  difficulty: "advanced",
  mode: "contextual",
  focusPage: "help",
  category: "Architecture",
  tags: ["graphql", "architecture", "subscriptions", "apollo", "real-time", "advanced"],
  prerequisites: ["getting-started"],
  learningObjectives: [
    "Understand why GraphQL was added alongside the existing REST API",
    "Know the backend schema structure: 30 queries, 23 mutations, 5 subscriptions",
    "Understand the dual-transport architecture (HTTP for queries, WebSocket for subscriptions)",
    "See how real-time subscriptions replace polling patterns",
    "Know the frontend hook library and how to use it",
    "Understand the codegen pipeline from Rust types to TypeScript",
    "Know the security and rate limiting protections",
  ],
  steps: [
    {
      id: "intro",
      title: "Why GraphQL?",
      content: `The runner already has a comprehensive REST API (69 modules, 400+ endpoints on port 9876). **So why add GraphQL?**

Three problems the REST API couldn't solve efficiently:

1. **Polling waste** — Components polled REST endpoints every 2-10 seconds for status changes. Most polls returned unchanged data. GraphQL subscriptions push changes over WebSocket the moment they happen.

2. **Over-fetching** — REST endpoints return fixed response shapes. A task list needs 5 fields but gets 35. GraphQL lets the client request exactly the fields it needs.

3. **Type safety gap** — REST responses are untyped JSON parsed with \`as TaskRun\`. GraphQL has a schema contract enforced on both sides, with auto-generated TypeScript types.

The GraphQL layer doesn't replace REST — it **coexists**. REST handles existing integrations; GraphQL handles the frontend's data needs.`,
      estimatedDuration: 1.5,
      tips: [
        "The REST API continues to serve all existing endpoints unchanged",
        "GraphQL is mounted at /graphql (HTTP) and /graphql/ws (WebSocket) on the same port",
      ],
    },
    {
      id: "backend-schema",
      title: "Backend Schema — The Type Contract",
      content: `The GraphQL schema is defined in Rust using **async-graphql v7** derives. The schema lives at \`src-tauri/src/graphql/\`:

| File | Role |
|------|------|
| \`types.rs\` | 28 GraphQL types with domain converters |
| \`query.rs\` | 30 query resolvers |
| \`mutation.rs\` | 23 mutation resolvers |
| \`subscription.rs\` | 5 subscription streams |
| \`schema.rs\` | Schema builder, SDL export, handlers |
| \`schema.graphql\` | Auto-generated 722-line SDL |

The schema is built at server startup with \`ApiState\` as context data, so every resolver has access to the database, event broadcaster, and configuration.`,
      estimatedDuration: 1,
      details: `**Type mapping approach:**

Rust structs decorated with \`#[derive(SimpleObject)]\` become GraphQL types. Domain types like \`TaskRun\` get a GraphQL mirror (\`GqlTaskRun\`) with explicit conversion via \`from_db()\` — this keeps the GraphQL layer decoupled from the database layer.

Enums use \`#[derive(Enum)]\` and get explicit match arms (no wildcards) so adding a new variant is a compile error until all conversions are updated.

Union types (\`RunnerEvent\`) enable polymorphic subscriptions where different event types flow through the same stream.`,
      tips: [
        "The SDL is auto-exported by a Rust test and used by the frontend codegen",
        "schema.graphql is committed to the repo for reference but regenerated on change",
      ],
    },
    {
      id: "resolver-pattern",
      title: "Resolver Delegation Pattern",
      content: `Every GraphQL resolver **delegates to existing database functions** — no new business logic lives in the GraphQL layer.

The standard pattern:

\`\`\`rust
async fn task_runs(&self, ctx: &Context<'_>, limit: i32) -> Result<Vec<GqlTaskRun>> {
    let state = ctx.data::<Arc<ApiState>>()?;
    // Try PG first, fall back to SQLite
    let runs = 'pg: {
        if let Some(pg) = &state.app_state.pg_db {
            match pg.get_task_runs(limit).await {
                Ok(r) => break 'pg r,
                Err(e) => warn!("PG failed, falling back: {}", e),
            }
        }
        let db = state.app_state.checkpoint_db.clone();
        tokio::task::spawn_blocking(move || db.get_task_runs(limit))
            .await??
    };
    Ok(runs.into_iter().map(GqlTaskRun::from_db).collect())
}
\`\`\`

Key details:
- **PG-first, SQLite fallback** — prefers PostgreSQL when available
- **spawn_blocking for SQLite** — prevents blocking the async runtime
- **Type conversion at the boundary** — \`from_db()\` maps database structs to GraphQL types`,
      estimatedDuration: 1.5,
      tips: [
        "Mutations follow the same pattern but additionally handle side effects (kill processes, broadcast events, release locks)",
        "The classify_error function maps error strings to typed error codes (13 variants)",
      ],
    },
    {
      id: "subscriptions",
      title: "Real-Time Subscriptions",
      content: `The 5 subscription resolvers replace polling with server-pushed events:

| Subscription | Replaces | Transport |
|-------------|----------|-----------|
| \`uiBridgeHealthStream\` | Polling GET /health every 10s | Timer-based push |
| \`runnerEvents\` | Browser WebSocket /ws/events | Broadcast channel filter |
| \`taskRunProgress\` | Polling GET /task-runs/:id every 3s | Broadcast channel + task filter |
| \`findingUpdates\` | Polling GET /findings/:id every 2s | DB poll with hash dedup |
| \`orchestrationLoopStatusStream\` | Polling loop status every 3s | Timer-based push |

**Two subscription strategies:**

1. **Broadcast-based** (runnerEvents, taskRunProgress) — filters the runner's event broadcast channel. Zero polling, instant delivery.
2. **DB-polling with dedup** (findingUpdates) — polls the DB at a configurable interval but only emits when a change hash differs. Necessary when the data source isn't event-driven.`,
      estimatedDuration: 1.5,
      details: `**Subscription lifecycle management:**

Subscriptions have built-in TTL and auto-close:
- \`findingUpdates\`: 30-minute TTL, auto-closes 2 ticks after task reaches terminal status
- \`orchestrationLoopStatusStream\`: 2-hour TTL, auto-closes when loop phase is Complete/Stopped/Error
- Broadcast-based subscriptions close when the client disconnects (handled by graphql-ws protocol)

The dedup hash for findingUpdates incorporates finding count, IDs, statuses, and updated_at timestamps — it only emits when something actually changed, avoiding unnecessary WebSocket traffic.`,
      tips: [
        "All subscriptions accept a minimum interval of 500ms to prevent abuse",
        "The frontend skip parameter disables subscriptions for terminal tasks automatically",
      ],
    },
    {
      id: "dual-transport",
      title: "Dual Transport Architecture",
      content: `The GraphQL system uses two transports:

**HTTP (POST /graphql)** — queries and mutations
- Standard request-response over the existing Axum router
- Concurrency limited to 20 simultaneous requests
- Trace ID propagated via X-Trace-Id header

**WebSocket (ws://…/graphql/ws)** — subscriptions
- Uses the \`graphql-ws\` protocol (not the older subscriptions-transport-ws)
- Excluded from the concurrency limit (long-lived by design)
- Lazy connection — only established when the first subscription is created

The **Apollo Client split link** routes operations automatically: if the operation is a subscription, it goes over WebSocket; everything else goes over HTTP. The frontend code doesn't need to think about transport.`,
      estimatedDuration: 1,
      tips: [
        "The WebSocket client retries up to 5 times with backoff on disconnect",
        "If WebSocket fails, health monitoring falls back to REST polling at 30s intervals",
      ],
    },
    {
      id: "frontend-apollo",
      title: "Frontend Apollo Client",
      content: `The Apollo Client is configured in \`src/lib/graphql-client.ts\` as a lazy singleton:

- **Created on first use** via \`getGraphQLClient()\` — no connection until needed
- **Split link** routes subscriptions to WS, queries/mutations to HTTP
- **ErrorLink** intercepts errors, logs with operation name, detects circuit breaker conditions
- **InMemoryCache** with entity normalization (\`keyFields: ["id"]\` for all entity types)
- **DevTools** enabled in development for Apollo Client DevTools browser extension

The \`ApolloProvider\` wraps the entire app in \`App.tsx\`, outside UIBridgeProvider but inside React Query's \`QueryClientProvider\`. This means both Apollo and React Query are available everywhere — they coexist.`,
      estimatedDuration: 1,
      details: `**Cache configuration:**

The cache uses type-specific policies:
- Entity types (GqlTaskRun, GqlWorkflow, GqlFinding): \`keyFields: ["id"]\` — normalized by ID
- Singleton types (UiBridgeHealth): \`keyFields: []\` — always merge into one cache entry
- GqlTaskRunOutput: \`keyFields: ["taskRunId"]\` — one cache entry per task

Delete mutations explicitly evict the deleted entity from cache and run \`cache.gc()\` to clean up orphaned references.`,
    },
    {
      id: "hooks-library",
      title: "Typed Hooks Library",
      content: `The hooks library at \`src/hooks/graphql/\` provides **27 typed hooks** organized by purpose:

**Query hooks** (11) — data fetching with Apollo's cache-and-network strategy
\`useTaskRunsGql\`, \`useTaskRunGql\`, \`useTaskRunOutputGql\`, \`useWorkflowsGql\`, \`useWorkflowGql\`, \`useFindingsGql\`, \`useFindingSummaryGql\`, \`useRunnerStatusGql\`, etc.

**Mutation hooks** (9) — state changes with automatic cache updates
\`useStopTaskRunGql\`, \`usePauseTaskRunGql\`, \`useCreateTaskRunGql\`, \`useRunWorkflowGql\`, etc.

**Subscription hooks** (5) — real-time streams
\`useHealthStream\`, \`useRunnerEvents\`, \`useTaskRunProgress\`, \`useFindingUpdates\`, \`useOrchestrationLoopStream\`

**Composite hooks** (2) — higher-level abstractions
\`useTaskRunControls()\` — stop/pause/unpause/delete with error handling
\`useTaskRunLive(taskId)\` — combines query + 2 subscriptions + output pagination`,
      estimatedDuration: 1.5,
      tips: [
        "Import everything from '@/hooks/graphql' — the barrel export covers all modules",
        "The Gql suffix distinguishes GraphQL hooks from existing React Query hooks",
      ],
    },
    {
      id: "subscription-migration",
      title: "How Polling Was Replaced",
      content: `Five components were migrated from polling to subscription-driven updates:

| Component | Before | After |
|-----------|--------|-------|
| \`useRunnerConnectionStatus\` | 10s HTTP poll | Health subscription (5s push) with 30s REST fallback |
| \`OrchestrationLoopBanner\` | 3s setInterval | Subscription detects phase changes, triggers Tauri fetch |
| \`useAiTaskPolling\` | 5s HTTP poll | Subscription for instant completion detection, 15s poll fallback |
| \`useTaskRuns / useTaskRun\` | 3s React Query poll | Subscription invalidates React Query cache on change |
| \`useDashboardState\` | 2-10s setInterval | Subscription triggers refresh only when data changes |

The pattern: **subscription drives cache invalidation, existing data source serves the data.** This is incremental — no big-bang migration required.`,
      estimatedDuration: 1.5,
      details: `**The hybrid pattern in detail:**

Rather than replacing React Query hooks entirely, the migration adds a GraphQL subscription that *invalidates the React Query cache* when data changes. This means:

1. React Query still fetches data from the Tauri IPC / REST API (existing, tested code)
2. The subscription just tells React Query *when* to refetch (event-driven, not time-based)
3. Polling is kept as a slow fallback (10-30s) in case the WebSocket drops

This avoids the risk of switching the data source while still getting the latency benefit of subscriptions.`,
    },
    {
      id: "codegen-pipeline",
      title: "Type Generation Pipeline",
      content: `TypeScript types are auto-generated from the Rust GraphQL schema:

\`\`\`
Rust types (async-graphql derives)
    ↓ cargo test schema_exports_valid_sdl
schema.graphql (722-line SDL)
    ↓ npm run graphql:codegen
src/generated/graphql.ts (855 lines of TypeScript types)
\`\`\`

**Three npm scripts:**
- \`npm run graphql:schema\` — exports SDL from Rust test
- \`npm run graphql:codegen\` — generates TypeScript from SDL + documents
- \`npm run graphql:check\` — validates generated output matches committed file (CI-safe)

The codegen reads both the SDL schema and the gql documents in \`hooks/graphql/documents.ts\` to generate precise operation types (not just schema types).`,
      estimatedDuration: 1,
      tips: [
        "Run graphql:schema after changing Rust types, then graphql:codegen after changing documents",
        "The Rust test also validates 21 assertions about the SDL (types, resolvers, enums present)",
      ],
    },
    {
      id: "security",
      title: "Security and Rate Limiting",
      content: `The GraphQL endpoint has multiple protection layers:

| Layer | Protection |
|-------|-----------|
| **Query depth** | Limit 12 — prevents deeply nested query attacks |
| **Query complexity** | Limit 500 — prevents single-query resource exhaustion |
| **Concurrency** | Max 20 concurrent HTTP requests (WS excluded) |
| **Introspection** | Disabled in release builds (\`#[cfg(not(dev))]\`) |
| **Subscription TTL** | 30min for findings, 2h for loop status — prevents resource leaks |
| **Error classification** | 13 typed error codes for machine-readable error handling |

The API is localhost-only (same as REST), so CORS is permissive. Security comes from binding to localhost, not from authentication headers.`,
      estimatedDuration: 1,
    },
    {
      id: "graphiql",
      title: "GraphiQL Playground",
      content: `In development mode, navigate to **http://localhost:9876/graphql** in your browser to access the **GraphiQL IDE**.

The playground provides:
- **Query editor** with syntax highlighting and autocomplete
- **Results panel** showing JSON responses
- **Schema explorer** (Docs sidebar) with all types and their fields
- **Subscription testing** — run subscriptions and see live events
- **History** of previous queries

**Quick smoke test:**

\`\`\`graphql
mutation { ping }
# Returns: {"data": {"ping": "pong"}}

{ runnerStatus { apiPort executorRunning configLoaded } }
# Returns current runner state
\`\`\`

The playground is disabled in release builds (introspection off).`,
      estimatedDuration: 1,
      tips: [
        "GraphiQL is served from the same Axum router as the REST API — no additional server needed",
        "The playground configures both the HTTP and WS endpoints automatically",
      ],
    },
    {
      id: "live-panel",
      title: "TaskRunLivePanel — Zero-Polling Task View",
      content: `The \`TaskRunLivePanel\` component demonstrates the full GraphQL stack in action:

- **useTaskRunLive(taskRunId)** composite hook provides all data
- **useTaskRunGql** query fetches initial task metadata
- **useTaskRunProgress** subscription streams live step/phase events
- **useFindingUpdates** subscription streams finding changes in real-time
- **useTaskRunOutputGql** query provides paginated output (10KB chunks)
- **useTaskRunControls** hook provides stop/pause/resume/delete mutations

The panel appears in the **Run Summary** page when a running task is selected. In compact mode (embedded in Run Summary), it hides output and findings detail. The full mode shows everything including a scrollable output preview and severity-colored findings list.

Events are accumulated with a **500-event ring buffer** to prevent memory growth during long-running tasks.`,
      estimatedDuration: 1.5,
      tips: [
        "The panel automatically hides when the task reaches a terminal state",
        "Subscriptions deactivate (skip=true) for terminal tasks to release resources",
      ],
    },
    {
      id: "mental-model",
      title: "The Mental Model",
      content: `**The GraphQL layer is a typed, real-time view of the runner's state.**

Think of it in three layers:

1. **Schema contract** — Rust types define the data model. The SDL is the source of truth. TypeScript types are derived, not hand-written.

2. **Transport** — HTTP for one-shot data, WebSocket for streams. The client doesn't choose — the split link routes automatically based on operation type.

3. **Integration strategy** — Subscriptions drive React Query cache invalidation. Existing data fetching stays. Polling becomes fallback, not primary. Each component can adopt GraphQL incrementally.

**What's NOT in GraphQL (by design):**
- File uploads (use REST multipart)
- SSE streaming (existing WS handles this)
- Internal Tauri IPC (stays as invoke() for desktop-specific features)
- Workflow execution triggers (complex setup logic stays in REST)

The GraphQL layer handles **reads, simple writes, and real-time monitoring**. Complex orchestration stays in the REST layer.`,
      estimatedDuration: 1.5,
      resources: [
        {
          title: "async-graphql docs",
          url: "https://async-graphql.github.io/async-graphql/en/",
          type: "documentation",
        },
        {
          title: "Apollo Client v4 docs",
          url: "https://www.apollographql.com/docs/react/",
          type: "documentation",
        },
        {
          title: "graphql-ws protocol",
          url: "https://github.com/enisdenjo/graphql-ws",
          type: "documentation",
        },
      ],
    },
  ],
};
