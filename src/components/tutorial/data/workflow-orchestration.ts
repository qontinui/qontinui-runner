/**
 * Workflow Orchestration Tutorial (Informational)
 *
 * Explains the Inngest-inspired workflow orchestration system: event bus,
 * flow control, durable queue, circuit breaker, phase timeouts, and
 * observability. Navigates to the Event History page to show live state.
 */

import type { Tutorial } from "../../../types/tutorial";

export const workflowOrchestrationTutorial: Tutorial = {
  id: "workflow-orchestration",
  title: "Workflow Orchestration Deep Dive",
  description:
    "Understand the event-driven orchestration system: how workflows communicate, how concurrency and rate limits are enforced, how the durable queue survives crashes, and how the circuit breaker prevents cascading failures.",
  duration: "12 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  focusPage: "gui-automation",
  category: "Architecture",
  tags: ["featured", "architecture", "inngest", "event-bus", "flow-control", "queue", "circuit-breaker"],
  prerequisites: ["getting-started"],
  learningObjectives: [
    "Understand the event bus and how workflows communicate",
    "Configure per-workflow flow control (concurrency, throttle, debounce, rate limit)",
    "Monitor the durable queue and understand crash recovery",
    "Interpret the circuit breaker state and how it prevents cascading failures",
    "Set per-phase timeouts to prevent hung workflows",
    "Use the Event History page for observability",
  ],
  steps: [
    // =========================================================================
    // Step 1: Big picture
    // =========================================================================
    {
      id: "intro",
      title: "Workflow Orchestration System",
      content: `The runner has a built-in workflow orchestration system inspired by **Inngest**, a durable workflow engine. Instead of running Inngest as a separate server, these patterns are implemented natively in Rust inside the runner.

**What this gives you:**
- **Event-driven chaining** — workflows trigger other workflows
- **Flow control** — per-workflow concurrency, throttle, debounce, rate limits
- **Durable queue** — queued workflows survive crashes
- **Circuit breaker** — prevents retrying into a failing system
- **Phase timeouts** — per-phase deadlines prevent hung workflows

Let's walk through each concept.`,
      estimatedDuration: 1,
      tips: [
        "This system runs entirely inside the runner — no external services needed",
        "All state is persisted to SQLite (and optionally PostgreSQL)",
      ],
    },

    // =========================================================================
    // Step 2: Event bus
    // =========================================================================
    {
      id: "event-bus",
      title: "The Workflow Event Bus",
      content: `At the core is an **in-process pub/sub event bus**. When a workflow completes, fails, or reaches a milestone, it emits a typed event.

**Lifecycle events emitted automatically:**
- \`workflow.completed\` — a workflow finished successfully
- \`workflow.failed\` — a workflow failed
- \`workflow.started\` — a workflow began executing
- \`step.completed\` / \`step.failed\` — individual phases
- \`verification.passed\` / \`verification.failed\`

**Who listens:**
- The **trigger system** subscribes to the broadcast channel and fires WorkflowChain triggers reactively
- Other workflows can **waitForEvent** — pause until a specific event arrives (with timeout)
- The **Event History page** polls the event API to display them`,
      details: `**Technical details:**

The event bus is a singleton (\`get_workflow_event_bus()\`) wrapping:
- A \`tokio::broadcast\` channel for fan-out to all listeners
- A \`VecDeque\` history buffer (max 500 events) for the API
- One-shot channels for \`waitForEvent\` callers
- An \`RwLock<Vec<EventSubscription>>\` for pattern-matched subscriptions

Events use dotted naming (\`workflow.completed\`) with glob matching (\`workflow.*\` matches any workflow event).

**Source files:**
- \`src-tauri/src/workflow_event_bus.rs\`
- \`src-tauri/src/unified_workflow_executor/task_lifecycle.rs\` (emission points)
- \`src-tauri/src/trigger_system/service.rs\` (broadcast subscriber)`,
      estimatedDuration: 2,
      tips: [
        "Events are in-memory — they're cleared when the runner restarts",
        "The broadcast channel has capacity 256; slow consumers may miss events",
      ],
    },

    // =========================================================================
    // Step 3: Event History page
    // =========================================================================
    {
      id: "event-history-page",
      title: "The Event History Page",
      content: `Open the **Event History** page to see the orchestration system in action.

In the sidebar, look for **Event History** under the Schedule section (near Triggers and Scheduled Tasks).

This page shows:
- **Queue status** — pending/running counts, max concurrent, durable indicator
- **Circuit breaker** — current state (Closed/Open/Half-Open), failure count
- **Event table** — all emitted events, newest first, with color coding
- **Active subscriptions** — which event patterns are being listened to

Everything auto-refreshes every 5 seconds.`,
      action: "Click Event History in the sidebar",
      targetElement: {
        selector: '[data-tutorial-id="sidebar"]',
        highlightType: "border",
        position: "right",
        allowInteraction: true,
      },
      estimatedDuration: 1,
    },

    // =========================================================================
    // Step 4: Queue status
    // =========================================================================
    {
      id: "queue-status",
      title: "The Durable Workflow Queue",
      content: `The **Queue Status** widget shows the state of the workflow execution queue.

**Key metrics:**
- **Pending** — workflows waiting to execute
- **Running** — currently executing (limited by the semaphore)
- **Max Concurrent** — how many can run in parallel (default: 3)
- **Durable** indicator — green means the queue is persisted to SQLite

**Crash recovery:** When the runner starts, it calls \`queue_recover_crashed()\` which resets any entries that were "running" back to "pending", then reloads them into the in-memory queue. No queued work is lost.`,
      targetElement: {
        selector: '[data-tutorial-id="queue-status-widget"]',
        highlightType: "spotlight",
        position: "bottom",
      },
      details: `**Technical details:**

The queue uses a \`tokio::Semaphore\` for concurrency control and a \`VecDeque\` for ordering. High-priority workflows are inserted at the front.

DB operations (\`queue_ops.rs\`):
- \`queue_insert()\` — persist on submit
- \`queue_update_status()\` — track running/completed/failed
- \`queue_recover_crashed()\` — reset running→pending on startup
- \`queue_cleanup()\` — remove old terminal entries (>7 days)

Both SQLite and PostgreSQL backends are supported.

**Source files:**
- \`src-tauri/src/workflow_queue.rs\`
- \`src-tauri/src/database/queue_ops.rs\`
- \`src-tauri/src/database/pg/queued_workflows.rs\``,
      estimatedDuration: 1,
      tips: [
        "The max concurrent default is 3 — you can change this in workflow settings via Flow Control",
        "The queue depth limit is 50 — beyond that, new submissions are rejected",
      ],
    },

    // =========================================================================
    // Step 5: Circuit breaker
    // =========================================================================
    {
      id: "circuit-breaker",
      title: "The Circuit Breaker",
      content: `The **Circuit Breaker** prevents cascading failures. It tracks consecutive workflow failures across the system.

**States:**
- 🟢 **Closed** — normal operation, all requests flow through
- 🔴 **Open** — tripped after too many failures, all retries immediately rejected
- 🟡 **Half-Open** — after cooldown, one probe request is allowed through

**When it trips:** After 5 consecutive failures (configurable), the breaker opens. This prevents hammering a failing API or broken dependency.

**Recovery:** After the cooldown period (default: 15-60s), a single probe request is allowed. If it succeeds, the breaker closes. If it fails, it reopens.`,
      targetElement: {
        selector: '[data-tutorial-id="circuit-breaker-widget"]',
        highlightType: "spotlight",
        position: "bottom",
      },
      details: `**Technical details:**

The \`CircuitBreaker\` struct uses \`std::sync::Mutex\` for thread-safe state management:
- \`record_success()\` — resets failure count, closes circuit
- \`record_failure()\` — increments count, opens circuit if threshold hit
- \`allow_request()\` — checks state, transitions Open→HalfOpen after cooldown

Called from \`task_lifecycle.rs\`: \`mark_task_completed()\` records success, \`mark_task_failed()\` records failure.

The circuit breaker is stored in \`AppState\` and accessed globally.

**Source file:** \`src-tauri/src/orchestrator/retry.rs\``,
      estimatedDuration: 1,
      tips: [
        "The circuit breaker state is also exposed via GET /inngest/circuit-breaker",
        "Non-retriable errors (auth failures, invalid config) never count toward the breaker",
      ],
    },

    // =========================================================================
    // Step 6: Flow control
    // =========================================================================
    {
      id: "flow-control",
      title: "Per-Workflow Flow Control",
      content: `Each workflow can have its own **flow control** settings, configured in the workflow editor.

**Four types of flow control:**

| Type | Behavior | Use Case |
|------|----------|----------|
| **Concurrency** | Limits simultaneous runs | Prevent resource contention |
| **Throttle** | Limits runs per time window (queues excess) | Respect API rate limits |
| **Rate Limit** | Hard cap per window (drops excess) | Prevent abuse |
| **Debounce** | Only runs the latest trigger in window | Avoid redundant runs |

All are optional and can be scoped by key (e.g., per-repo, per-user).`,
      details: `**How it works at runtime:**

When a workflow is about to start, \`loop_controller.rs\` loads its \`flow_control_json\` from the DB and calls \`FlowControlEnforcer.check()\`:

- **Allow** → proceed to execution
- **Wait** → hold in queue, retry after \`wait_ms\`
- **Skip** → drop the execution (rate limit exceeded)

The enforcer maintains per-key state:
- \`concurrency_counts\` — HashMap tracking active runs
- \`throttle_windows\` — sliding window of timestamps
- \`rate_limit_windows\` — sliding window (drops instead of queuing)
- \`debounce_timers\` — last trigger timestamp per key

**Source file:** \`src-tauri/src/flow_control.rs\``,
      estimatedDuration: 2,
      tips: [
        "Configure flow control in the workflow editor's Settings → Flow Control section",
        "Throttle queues excess; rate limit drops them — choose based on your needs",
        "Debounce is great for file-watcher triggers that fire rapidly",
      ],
    },

    // =========================================================================
    // Step 7: Phase timeouts
    // =========================================================================
    {
      id: "phase-timeouts",
      title: "Phase Timeouts",
      content: `Each phase of a workflow has a configurable **timeout deadline**. If a phase exceeds its timeout, the workflow is stopped with a \`PhaseTimeout\` reason.

**Default timeouts:**
- **Setup** — 5 minutes
- **Execution** (agentic) — 30 minutes
- **Verification** — 10 minutes
- **Completion** — 5 minutes
- **AI Summary** — 3 minutes

You can override these per-workflow in the Flow Control settings panel.`,
      details: `**How timeouts are enforced:**

1. \`TaskOrchestrator.state_entered_at\` records when each phase starts (ISO-8601 timestamp)
2. \`check_timeout()\` computes elapsed time vs the phase's configured timeout
3. \`loop_handlers.rs\` calls \`check_timeout()\` in \`handle_check_preconditions()\` every iteration
4. If exceeded, the loop completes with \`CompletionReason::PhaseTimeout\`

The timeout config is stored as \`phase_timeouts_json\` on the workflow and parsed into \`PhaseTimeouts\` in \`LoopConfig::from_workflow()\`.

**Source files:**
- \`src-tauri/src/orchestrator/state.rs\` (PhaseTimeouts struct + check_timeout)
- \`src-tauri/src/unified_workflow_executor/loop_handlers.rs\` (enforcement)
- \`src-tauri/src/unified_workflow_executor/types.rs\` (parsing from DB)`,
      estimatedDuration: 1,
      tips: [
        "The execution phase timeout is the most impactful — it governs the main AI work loop",
        "Timeouts use chrono for clock-safe comparison with .max(0) to handle clock skew",
      ],
    },

    // =========================================================================
    // Step 8: Non-retriable errors
    // =========================================================================
    {
      id: "non-retriable",
      title: "Smart Error Classification",
      content: `Not all errors should be retried. The system classifies errors into **retriable** and **non-retriable** categories.

**Non-retriable patterns** (immediate failure, no retry):
- Authentication failures, permission denied
- Invalid configuration, schema validation errors
- Compilation failures, syntax errors
- Resource not found (the thing you're looking for doesn't exist)

**Retriable patterns** (exponential backoff with jitter):
- Timeouts, connection errors
- Rate limit responses (HTTP 429)
- Server errors (503, 504)
- Network resets

This prevents wasting retries on errors that will never succeed.`,
      details: `**Technical details:**

\`is_non_retriable(error)\` checks the error message against 9 patterns (case-insensitive):
"syntax error", "type error", "compilation failed", "invalid configuration", "authentication failed", "permission denied", "resource not found", "schema validation", "missing required field"

The retry service uses proper \`rand::rng()\` for jitter (not time-based pseudo-random) and supports configurable backoff: base_delay_ms, max_delay_ms, exponential_base.

**Source file:** \`src-tauri/src/orchestrator/retry.rs\``,
      estimatedDuration: 1,
    },

    // =========================================================================
    // Step 9: Observability
    // =========================================================================
    {
      id: "observability",
      title: "Observability and Tracing",
      content: `Every workflow execution produces **enriched execution spans** stored in the database. These go beyond basic start/end times:

**Enrichment fields (added to execution_spans table):**
- \`queue_wait_ms\` — time spent waiting in the queue before execution
- \`retry_attempt\` — which retry this is (0 = first attempt)
- \`phase\` — which workflow phase (setup, verification, agentic, completion)
- \`iteration\` — loop iteration number
- \`workflow_id\` — which workflow definition produced this span

This data enables questions like:
- "How long do workflows wait in the queue?"
- "Which phase takes the longest?"
- "How many retries does this workflow typically need?"`,
      details: `**Database schema:**

Migration v146 added these columns to \`execution_spans\` (both SQLite and PostgreSQL):

\`\`\`sql
ALTER TABLE execution_spans ADD COLUMN queue_wait_ms INTEGER;
ALTER TABLE execution_spans ADD COLUMN retry_attempt INTEGER;
ALTER TABLE execution_spans ADD COLUMN phase TEXT;
ALTER TABLE execution_spans ADD COLUMN iteration INTEGER;
ALTER TABLE execution_spans ADD COLUMN workflow_id TEXT;
\`\`\`

The tracing layer (\`tracing_layers.rs\`) extracts these from span attributes and writes them alongside the standard span fields.`,
      estimatedDuration: 1,
    },

    // =========================================================================
    // Step 10: API endpoints
    // =========================================================================
    {
      id: "api-endpoints",
      title: "MCP API Endpoints",
      content: `The orchestration system exposes 5 API endpoints for monitoring and integration:

| Endpoint | Returns |
|----------|---------|
| \`GET /inngest/events?limit=N\` | Recent workflow events |
| \`GET /inngest/subscriptions\` | Active event subscriptions |
| \`GET /inngest/queue\` | Queue status (pending, running, limits) |
| \`GET /inngest/circuit-breaker\` | Circuit breaker state and failure count |
| \`GET /inngest/flow-control\` | Flow control enforcer status |

All return the standard \`{ success: true, data: {...} }\` envelope.

You can also verify API contracts using the spec runner:
\`POST /ui-bridge/specs/verify-api\` with \`{ specId: "event-history" }\``,
      estimatedDuration: 1,
      tips: [
        "The Event History page polls these endpoints every 5 seconds",
        "You can test endpoints directly with curl from the terminal",
      ],
      resources: [
        { title: "Inngest (inspiration)", url: "https://www.inngest.com/docs" },
      ],
    },

    // =========================================================================
    // Step 11: Data flow summary
    // =========================================================================
    {
      id: "data-flow",
      title: "How It All Fits Together",
      content: `Here's the complete data flow when a workflow executes:

1. **Submit** → workflow enters the durable queue (persisted to SQLite)
2. **Dequeue** → semaphore permit acquired, flow control checked
3. **Execute** → phases run with per-phase timeout enforcement
4. **Events** → lifecycle events emitted to the event bus
5. **Triggers** → trigger system listens for events, fires chain triggers
6. **Retry** → failures checked against non-retriable patterns and circuit breaker
7. **Complete** → queue entry marked terminal, execution spans enriched

**The key insight:** Every piece is independent but composable. You can use the durable queue without flow control, or the event bus without phase timeouts. They layer on top of the existing execution engine.`,
      estimatedDuration: 1,
    },

    // =========================================================================
    // Step 12: Wrap up
    // =========================================================================
    {
      id: "summary",
      title: "What You've Learned",
      content: `You now understand the workflow orchestration system:

✅ **Event Bus** — pub/sub for cross-workflow communication
✅ **Flow Control** — concurrency, throttle, debounce, rate limit per workflow
✅ **Durable Queue** — crash-safe workflow queuing with auto-recovery
✅ **Circuit Breaker** — prevents cascading failures
✅ **Phase Timeouts** — per-phase deadlines for hung workflow prevention
✅ **Smart Retries** — non-retriable classification + jitter backoff
✅ **Observability** — enriched execution spans + API endpoints

**Next steps:**
- Configure flow control on a workflow in the editor (Settings → Flow Control)
- Run a workflow and watch the Event History page update in real time
- Check the circuit breaker state after a failed workflow`,
      estimatedDuration: 1,
    },
  ],
};
