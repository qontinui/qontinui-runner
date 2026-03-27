import type { Tutorial } from "../../../types/tutorial";

/**
 * Informational tutorial explaining the Observation Memory system —
 * Engram-inspired persistent cross-session knowledge for AI workflows.
 */
export const observationMemoryTutorial: Tutorial = {
  id: "observation-memory",
  title: "Observation Memory: Cross-Session AI Knowledge",
  description:
    "Understand how the observation memory system captures, stores, and retrieves knowledge across workflow sessions — enabling your AI to learn from past runs.",
  duration: "12 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  focusPage: "help",
  category: "AI Features",
  tags: ["featured", "observations", "memory", "engram", "knowledge", "feedback"],
  prerequisites: ["getting-started"],
  learningObjectives: [
    "Understand what observation memory is and why it exists",
    "Learn the six observation types and when each is used",
    "Understand how topic-key upsert keeps knowledge evolving instead of accumulating",
    "See how observations flow into AI prompts and the knowledge graph",
    "Know how to search, create, and manage observations from the Memory tab",
    "Understand the auto-capture pipeline: task completion, session summaries, and learning patterns",
  ],

  steps: [
    // -----------------------------------------------------------------------
    // Step 1: The Big Picture
    // -----------------------------------------------------------------------
    {
      id: "what-is-observation-memory",
      title: "What Is Observation Memory?",
      content: `AI agents lose all context when a session ends. The observation memory system solves this by giving the runner **persistent, searchable, cross-session knowledge**.

Every decision made, bug fixed, pattern discovered, and architecture insight learned can be saved as a typed **observation**. Future sessions automatically retrieve relevant observations and inject them into AI prompts — so the AI doesn't repeat mistakes or rediscover known patterns.

The system is inspired by [Engram](https://github.com/Gentleman-Programming/engram), an open-source memory layer for AI coding agents, adapted to work natively with qontinui's PostgreSQL database, knowledge graph, and MCP tool ecosystem.`,
      estimatedDuration: 1.5,
      tips: [
        "Observations persist in PostgreSQL — they survive runner restarts, unlike in-memory context",
        "The system uses full-text search (tsvector) for relevance-ranked retrieval",
      ],
      details: `**Data Flow Overview:**

1. Knowledge enters the system via **auto-capture** (task completion, session end, learning patterns) or **manual creation** (Memory tab, MCP tools)
2. Observations are stored in PostgreSQL with deduplication, privacy stripping, and topic-key upsert
3. When a new AI session starts, relevant observations are fetched and injected into the prompt
4. The knowledge graph links observations to task runs and workflows for causal reasoning
5. A retention policy archives stale, low-value observations over time`,
    },

    // -----------------------------------------------------------------------
    // Step 2: The Memory Tab
    // -----------------------------------------------------------------------
    {
      id: "memory-tab-overview",
      title: "The Memory Tab",
      content: `The **Memory** tab in the sidebar's OBSERVE group is the primary interface for browsing observations. It shows:

- **Stats badges** at the top — counts by observation type, auto-loaded on page open
- **Search bar** — full-text search with PostgreSQL tsvector ranking
- **Create form** — manual observation creation with type selection
- **Results list** — expandable rows showing previews (300 characters), with full content on click

This is also where you can delete stale observations or verify what the AI "remembers."`,
      targetElement: {
        selector: '[data-tutorial-id="observation-browser"]',
        highlightType: "spotlight",
        position: "center",
        scrollIntoView: true,
      },
      estimatedDuration: 1,
      tips: [
        "Stats badges are clickable — clicking a type badge searches for observations of that type",
      ],
    },

    // -----------------------------------------------------------------------
    // Step 3: Observation Types
    // -----------------------------------------------------------------------
    {
      id: "observation-types",
      title: "Six Observation Types",
      content: `Every observation has a **type** that classifies what kind of knowledge it captures:

- **decision** — A choice made and why (e.g., "Chose JWT over session cookies because...")
- **architecture** — Structural insight about the system (e.g., "Auth middleware uses a pipeline pattern")
- **bugfix** — A problem and its solution (e.g., "Race condition in concurrent saves fixed by...")
- **pattern** — A recurring behavior or approach (e.g., "Verification always fails on first run after deploy")
- **learning** — General knowledge gained (e.g., "Session summaries improve when task events are included")
- **discovery** — Something new found (e.g., "The API supports batch operations we weren't using")

Types affect how observations are surfaced: **bugfix** observations are prioritized when the AI encounters errors, **architecture** observations appear during workflow generation.`,
      targetElement: {
        selector: '[data-tutorial-id="observation-stats"]',
        highlightType: "border",
        position: "bottom",
        scrollIntoView: true,
      },
      estimatedDuration: 1.5,
    },

    // -----------------------------------------------------------------------
    // Step 4: Topic-Key Upsert
    // -----------------------------------------------------------------------
    {
      id: "topic-key-upsert",
      title: "Evolving Knowledge with Topic Keys",
      content: `A key design principle: **knowledge should evolve, not accumulate**.

When an observation is saved with a **topic key** (e.g., \`architecture/auth-model\`), it doesn't create a new row every time. Instead, it **upserts** — updating the existing observation with that key and incrementing a \`revision_count\`.

This means:
- The AI always sees the **latest** understanding, not a pile of outdated entries
- Session summaries for the same workflow update in place
- Learning patterns refine over time as confidence grows

Observations without a topic key are stored as independent entries — useful for one-off findings or discoveries.`,
      estimatedDuration: 1,
      tips: [
        "The revision count is visible in search results — high-revision observations are actively maintained knowledge",
        "Topic keys follow a namespace pattern: type/subject (e.g., pattern/recurring/login-failure)",
      ],
      details: `**Deduplication Layer:**

Beyond topic-key upsert, there's a content-hash deduplication layer. If the exact same content (normalized: trimmed, lowercased) is saved within a 15-minute window, it increments a \`duplicate_count\` instead of creating a new row. This prevents noise from concurrent agents or rapid retries.`,
    },

    // -----------------------------------------------------------------------
    // Step 5: Privacy Stripping
    // -----------------------------------------------------------------------
    {
      id: "privacy-stripping",
      title: "Privacy Protection",
      content: `Observations may contain sensitive data from automation — API keys, credentials, internal URLs. The system strips these automatically.

Any content wrapped in \`<private>...</private>\` tags is replaced with \`[REDACTED]\` **before** the content is hashed or stored. This happens at two layers:
1. The PgDb storage layer (Rust) — defense in depth
2. The content hash computation — so redacted content doesn't affect deduplication

This means you can freely capture context that includes sensitive data, and the storage layer ensures it never persists.`,
      estimatedDuration: 1,
      tips: [
        "Privacy stripping happens before hashing — two observations with different secrets but same structure won't be detected as duplicates",
      ],
    },

    // -----------------------------------------------------------------------
    // Step 6: Full-Text Search
    // -----------------------------------------------------------------------
    {
      id: "full-text-search",
      title: "Searching Observations",
      content: `The search system uses **PostgreSQL tsvector** — a production-grade full-text search engine with:

- **Stemming** — searching "running" also matches "run", "runs", "runner"
- **Relevance ranking** — results are ordered by \`ts_rank\`, not just recency
- **Progressive disclosure** — search returns 300-character previews; click to see full content

This is the same search that powers the AI's memory retrieval. When a new session starts, the runner searches for observations relevant to the current workflow and injects the top results into the prompt.`,
      targetElement: {
        selector: '[data-tutorial-id="observation-search"]',
        highlightType: "spotlight",
        position: "bottom",
        scrollIntoView: true,
      },
      estimatedDuration: 1,
      tips: [
        "Search queries support natural language — try 'authentication failures' or 'performance regression'",
        "Project-scoped search is available via the API: /observations/context?project_id=X",
      ],
    },

    // -----------------------------------------------------------------------
    // Step 7: Auto-Capture Pipeline
    // -----------------------------------------------------------------------
    {
      id: "auto-capture-pipeline",
      title: "How Observations Are Captured Automatically",
      content: `Most observations enter the system **without manual input** through three auto-capture pipelines:

**1. Task Completion** — When a workflow task finishes, the runner saves the outcome as an observation: workflow name, status, iterations, duration, and key findings from \`task_knowledge\`. Topic key: \`task-outcome/{workflow-name}\`.

**2. Session Summaries** — When a terminal session ends (after 30+ seconds), a summary observation is saved with the session name, duration, and event counts (actions, errors, state changes). Topic key: \`session/{session-name}\`.

**3. Learning Bridge** — Every 5 minutes, the learning service syncs high-confidence patterns and insights to observations. Patterns with confidence >= 60% and 2+ occurrences are persisted. Topic key: \`pattern/{type}/{id}\`.

**4. Cross-Run Patterns** — After reflection analysis, recurring findings and fix oscillations are saved as observations. Topic key: \`cross-run/{workflow-name}\`.`,
      estimatedDuration: 1.5,
      details: `**Auto-Capture Architecture:**

- \`task_lifecycle.rs\` → \`auto_capture_observations()\` — fires on task completion via \`tokio::spawn\`
- \`session-summary-service.ts\` → subscribes to \`SessionManager\` lifecycle events
- \`learning-observation-bridge.ts\` → periodic 5-minute sync with confidence thresholds
- \`loop_controller.rs\` → saves cross-run pattern observations after \`post_run_analysis\`

All pipelines are non-blocking — failures are logged but never crash the runner.`,
    },

    // -----------------------------------------------------------------------
    // Step 8: Knowledge Graph Integration
    // -----------------------------------------------------------------------
    {
      id: "knowledge-graph-integration",
      title: "Observations in the Knowledge Graph",
      content: `Observations are first-class citizens in the knowledge graph alongside findings, fixes, and patterns.

The graph engine loads observations from PostgreSQL and creates:
- **Observation nodes** — with type, scope, topic key, and content preview as properties
- **LearnedFrom edges** — connecting observations to the task runs that produced them
- **InformedBy edges** — connecting workflows to observations that influenced them

This means causal reasoning works across observations: "This workflow succeeded because it was informed by an observation about a recurring auth failure pattern."

The graph summary endpoint (\`GET /graph/summary\`) includes observation node counts, and the graph analytics hooks in the frontend expose observation data alongside findings and fixes.`,
      estimatedDuration: 1,
      tips: [
        "Observation nodes appear in the graph summary stats alongside other entity types",
        "The graph loads observations via a single batch query (not N+1) for performance",
      ],
    },

    // -----------------------------------------------------------------------
    // Step 9: AI Prompt Injection
    // -----------------------------------------------------------------------
    {
      id: "ai-prompt-injection",
      title: "How Memory Reaches the AI",
      content: `The most impactful part of the system: **observations are automatically injected into AI prompts**.

When an agentic session starts (\`ai_session.rs\`), the runner:
1. Extracts the task/workflow name from the prompt
2. Searches observations for relevant matches via \`format_observation_memory_for_prompt()\`
3. Formats the top 10-15 results as a markdown "Memory Context" section
4. Prepends it to the enhanced prompt before sending to the AI

The AI sees something like:
\`\`\`
## Memory Context (from past sessions)
3 relevant observation(s) from previous work.

- **Task succeeded: login-flow** (rev 4) [task-outcome/login-flow] (id: 42, type: learning)
  Workflow completed in 3 iterations, 45s. Key finding: auth timeout...
\`\`\`

Workflow generation also gets this treatment — observations are injected into the generator's \`inline_context\` so the AI builder knows about past issues.`,
      estimatedDuration: 1.5,
      details: `**Injection Points:**

1. \`ai_session.rs:run_prompt()\` — Agentic sessions (uses \`prompt_name\` as search query)
2. \`unified_workflows.rs:generate_unified_workflow_handler()\` — Workflow generation (uses \`request.description\` as search query)
3. \`context/resolution.rs:format_observation_memory_for_prompt()\` — The shared formatting function

Both injection points are async and gracefully skip if PG is unavailable.`,
    },

    // -----------------------------------------------------------------------
    // Step 10: MCP Tools
    // -----------------------------------------------------------------------
    {
      id: "mcp-tools",
      title: "MCP Tools for AI Agents",
      content: `The observation system exposes **10 MCP tools** so AI agents (Claude Code, Cursor, etc.) can directly interact with memory:

| Tool | Purpose |
|------|---------|
| \`mem_save\` | Save a typed observation with optional topic key |
| \`mem_search\` | Full-text search with relevance ranking |
| \`mem_get\` | Get full content by ID (progressive disclosure) |
| \`mem_context\` | Get project-scoped observations |
| \`mem_update\` | Update title, content, or type |
| \`mem_delete\` | Soft-delete an observation |
| \`mem_stats\` | Type breakdown statistics |
| \`mem_by_task_run\` | Observations linked to a specific task run |
| \`mem_export\` | Export all observations as JSON |
| \`mem_cleanup\` | Run retention policy (archive old, low-value observations) |

Read-only tools (\`mem_search\`, \`mem_get\`, \`mem_context\`, \`mem_stats\`, \`mem_by_task_run\`, \`mem_export\`) are auto-approved. Write tools (\`mem_save\`, \`mem_update\`, \`mem_delete\`, \`mem_cleanup\`) require MODIFY permission.`,
      estimatedDuration: 1,
      tips: [
        "AI agents can save observations during agentic phases — capturing insights as they work",
        "The mem_cleanup tool is useful for maintenance: mem_cleanup(retention_days=90) archives old single-revision observations",
      ],
    },

    // -----------------------------------------------------------------------
    // Step 11: Creating Observations Manually
    // -----------------------------------------------------------------------
    {
      id: "manual-creation",
      title: "Creating Observations Manually",
      content: `While most observations are auto-captured, you can create them manually from the **Memory tab**:

1. Click the **+ New** button to open the create form
2. Enter a **title** (concise, searchable)
3. Enter **content** (markdown supported — use headers, lists, code blocks)
4. Select the **type** from the dropdown (decision, architecture, bugfix, pattern, learning, discovery)
5. Click **Save**

Manual observations are especially useful for:
- Recording architectural decisions that the AI should always know about
- Documenting workarounds or known issues
- Capturing domain knowledge that can't be auto-detected`,
      targetElement: {
        selector: '[data-tutorial-id="observation-create-btn"]',
        highlightType: "pulse",
        position: "bottom",
        scrollIntoView: true,
      },
      estimatedDuration: 1,
    },

    // -----------------------------------------------------------------------
    // Step 12: Retention & Maintenance
    // -----------------------------------------------------------------------
    {
      id: "retention-policy",
      title: "Retention Policy & Maintenance",
      content: `Over time, observations accumulate. The retention system keeps the knowledge base healthy:

- **High-value observations** (multiple revisions, recent updates) are kept indefinitely
- **Stale observations** (single revision, no duplicates, older than the retention window) are soft-deleted
- The default policy: archive after **90 days** if \`revision_count <= 1\` and \`duplicate_count = 0\`
- Soft-deleted observations are excluded from search but can be recovered

You can trigger cleanup manually via:
- The \`mem_cleanup\` MCP tool
- The \`POST /observations/cleanup?retention_days=90\` HTTP endpoint

The **export** feature (\`GET /observations/export\` or \`mem_export\` MCP tool) dumps all active observations as JSON for backup before cleanup.`,
      estimatedDuration: 1,
      tips: [
        "Run mem_export before a major cleanup to create a backup",
        "Observations with topic keys tend to have high revision counts and survive cleanup naturally",
      ],
    },

    // -----------------------------------------------------------------------
    // Step 13: Mental Model Summary
    // -----------------------------------------------------------------------
    {
      id: "mental-model",
      title: "Putting It All Together",
      content: `Here's the mental model for how observation memory works in qontinui:

**Input** (how knowledge enters):
- Auto-capture on task completion, session end, and learning sync
- Manual creation from Memory tab or MCP tools
- Cross-run pattern detection from reflection analysis

**Storage** (how knowledge persists):
- PostgreSQL with tsvector full-text search indexes
- Topic-key upsert for evolving knowledge
- Content-hash dedup for noise prevention
- Privacy stripping for sensitive data

**Output** (how knowledge is used):
- Injected into AI prompts at session start and workflow generation
- Visible in the knowledge graph as Observation nodes with causal edges
- Searchable from the Memory tab and MCP tools
- Feeds back into cross-run learning for pattern detection

The result: **your AI gets smarter over time**, learning from every workflow run without you having to manually maintain a knowledge base.`,
      estimatedDuration: 1,
      resources: [
        {
          title: "Engram (inspiration)",
          url: "https://github.com/Gentleman-Programming/engram",
        },
      ],
    },
  ],
};
