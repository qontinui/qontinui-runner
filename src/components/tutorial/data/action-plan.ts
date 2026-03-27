/**
 * Structured Action Plan Tutorial
 *
 * Informational tutorial explaining the structured action plan system —
 * how LLMs can drive UI automation through typed action sequences
 * instead of natural language interpretation.
 */

import type { Tutorial } from "../../../types/tutorial";

export const actionPlanTutorial: Tutorial = {
  id: "action-plan",
  title: "Structured Action Plans",
  description:
    "Learn how the structured action plan system enables precise, multi-step UI automation through typed actions, element resolution strategies, confidence filtering, and plan caching.",
  duration: "12 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  focusPage: "help",
  category: "Integration",
  tags: ["featured", "api", "automation", "integration", "skyvern"],
  prerequisites: ["getting-started"],
  learningObjectives: [
    "Understand how action plans bypass natural language interpretation for faster UI automation",
    "Know the four element resolution strategies and when to use each",
    "Use confidence filtering to prevent uncertain actions from executing",
    "Leverage plan caching for cross-run reuse of successful action sequences",
    "Apply token budget pruning to keep AI context within limits",
  ],
  steps: [
    {
      id: "intro",
      title: "What Are Structured Action Plans?",
      content: `The action plan system lets AI agents drive UI automation with **typed actions** instead of natural language.

**The problem it solves:**
When an AI agent calls \`sdk_ai_execute("Click the Submit button")\`, a second LLM call interprets that instruction. This adds latency, cost, and ambiguity — especially for multi-step interactions like filling a form.

**The solution:**
Action plans specify **exactly** what to do: click this element, type this text, select this option. Each action names its target, action type, and parameters. No interpretation needed.

This is inspired by [Skyvern's](https://github.com/Skyvern-AI/skyvern) structured action protocol, adapted for Qontinui's cooperative UI Bridge architecture.`,
      estimatedDuration: 1,
    },
    {
      id: "anatomy",
      title: "Anatomy of an Action Plan",
      content: `An action plan is a JSON object with an ordered array of actions:

\`\`\`json
{
  "actions": [
    {
      "action": "click",
      "target": { "elementId": "email-input" },
      "confidence": 0.95,
      "reasoning": "Focus the email field"
    },
    {
      "action": "type",
      "target": { "elementId": "email-input" },
      "params": { "text": "user@example.com" },
      "confidence": 0.95
    },
    {
      "action": "click",
      "target": { "searchText": "Submit", "elementType": "button" },
      "confidence": 0.9
    }
  ],
  "goal": "Fill and submit login form",
  "confidenceThreshold": 0.5,
  "stopOnFailure": true
}
\`\`\`

Actions execute **sequentially**. Each produces an independent result with success/failure, timing, and the resolved element ID.`,
      tips: [
        "Actions support 21 types: click, type, select, hover, focus, scroll, check, toggle, autocomplete, sendKeys, and more",
        "The 'wait' action pauses execution (capped at 30 seconds) for UI transitions",
        "Empty plans return immediate success — useful for health checks",
      ],
      details: `**Full action type list:**
click, doubleClick, rightClick, type, clear, select, check, uncheck, toggle, hover, focus, scroll, scrollIntoView, setValue, drag, submit, sendKeys, autocomplete, navigate, wait

**Response structure:**
Each action result contains: index, success, action, resolvedElementId, error, skippedLowConfidence, durationMs, and elementState (post-action DOM state).

The aggregate response includes: success (all-pass), goal, results array, executedCount, skippedCount, failedCount, totalDurationMs, and cached (whether stored for reuse).`,
      estimatedDuration: 2,
    },
    {
      id: "element-resolution",
      title: "Finding Elements: Four Strategies",
      content: `Each action specifies its target through an \`ElementTarget\` with multiple resolution strategies, tried in priority order:

**1. elementId** (fastest)
Direct registry lookup. Use element IDs from a prior snapshot.
\`{ "elementId": "button-submit-42" }\`

**2. testId** (stable across refactors)
Matches the \`data-testid\` DOM attribute via the find handler's native filter.
\`{ "testId": "login-submit" }\`

**3. selector** (precise)
CSS selector query.
\`{ "selector": "#main-form .submit-btn" }\`

**4. searchText** (flexible)
Fuzzy text search with optional type narrowing.
\`{ "searchText": "Submit", "elementType": "button" }\`

Different actions in the same plan can use different strategies.`,
      tips: [
        "elementId is fastest because it skips IPC entirely — use it when you have snapshot data",
        "testId is the best choice for test automation — data-testid attributes survive refactors",
        "searchText performs fuzzy matching against labels, text content, and accessible names",
      ],
      details: `**Resolution is per-action:** Each action resolves its target independently. If one action uses elementId and another uses searchText, each goes through its own resolution path.

**Failure behavior:** If all strategies fail, the action reports "Element resolution failed" and either stops the plan (stopOnFailure=true) or continues to the next action.

**Non-element actions:** \`navigate\` and \`wait\` bypass element resolution entirely. They don't need a target element.`,
      estimatedDuration: 2,
    },
    {
      id: "confidence-filtering",
      title: "Confidence Filtering",
      content: `Every action carries a **confidence score** (0.0–1.0) representing the AI's certainty that this is the correct action.

The plan-level \`confidenceThreshold\` (default: 0.5) controls filtering:
- **Above threshold:** Action executes normally
- **Below threshold:** Action is **skipped** — no interaction occurs

**Why this matters:**
When an LLM plans a 5-step form fill, it might be very confident about the first 4 steps but uncertain about the last. Confidence filtering prevents that uncertain action from clicking the wrong thing.

Skipped actions appear in the results with \`skippedLowConfidence: true\` and \`durationMs: 0\`. They don't even attempt element resolution, saving IPC overhead.`,
      tips: [
        "Actions without a confidence field default to 1.0 — they always execute",
        "Set confidenceThreshold to 0.0 to disable filtering entirely",
        "The threshold is checked before element resolution, so skipped actions are truly free",
      ],
      estimatedDuration: 1,
    },
    {
      id: "error-handling",
      title: "Error Handling and stopOnFailure",
      content: `Action plans offer two error handling modes:

**stopOnFailure: true** (default)
The plan halts at the first failed action. The results array contains only the actions that were attempted. Use this for dependent sequences where a failed step invalidates everything after it.

**stopOnFailure: false**
All actions execute regardless of individual failures. The response shows the full results array with mixed success/failure statuses. Use this for independent operations where partial success is acceptable.

**Safety limits:**
- Maximum 50 actions per plan (HTTP 400 if exceeded)
- Wait actions capped at 30 seconds
- IPC inherits circuit breaker protection (5+ failures in 30s opens the circuit)`,
      estimatedDuration: 1,
    },
    {
      id: "autocomplete-action",
      title: "The Autocomplete Action",
      content: `The \`autocomplete\` action handles inputs with suggestion dropdowns — a common pain point in UI automation.

**How it works:**
1. Types \`searchText\` into the target input to trigger suggestions
2. Polls for a dropdown to appear (up to \`suggestionTimeout\` ms)
3. Clicks the option matching \`selectValue\`

\`\`\`json
{
  "action": "autocomplete",
  "target": { "testId": "country-select" },
  "params": {
    "searchText": "united",
    "selectValue": "United States",
    "suggestionTimeout": 3000,
    "clear": true
  }
}
\`\`\`

**Framework coverage:**
Dropdown detection supports ARIA listbox, Radix/shadcn, MUI, Select2, Ant Design, Headless UI, and generic CSS patterns. The same detection powers the enhanced \`select\` action on combobox elements.`,
      tips: [
        "If no suggestion appears within the timeout, the action throws an error (not silent success)",
        "The select action on combobox elements retries up to 5 times with 50ms intervals for async frameworks",
        "clear defaults to true — existing input is cleared before typing",
      ],
      estimatedDuration: 1,
    },
    {
      id: "caching",
      title: "Plan Caching for Reuse",
      content: `Successful plans can be **cached** for cross-run reuse, skipping the expensive LLM planning call entirely.

**How to enable caching:**
Include \`pageUrl\` and \`elementSnapshot\` in the request:

\`\`\`json
{
  "actions": [...],
  "pageUrl": "http://localhost:3001/settings",
  "elementSnapshot": [
    { "id": "btn-save", "type": "button", "label": "Save" }
  ]
}
\`\`\`

**Cache keys:**
- URL is normalized (query string and fragment stripped)
- Elements are fingerprinted with FNV-1a hashes of id+type+role+label, sorted for order independence

**Lifecycle:**
- Successful plans are stored automatically (\`cached: true\` in response)
- Failed plans are evicted immediately
- LRU eviction at 100 entries
- Entries expire after 1 hour`,
      tips: [
        "Use the cache lookup endpoint to check for cached plans before calling the LLM",
        "The userDetailQuery/userDetailAnswer fields separate intent from data, enabling plan personalization",
        "GET /ui-bridge/control/action-plan/cache/stats shows entry count and hit rate",
      ],
      details: `**Cache endpoints:**

\`GET /ui-bridge/control/action-plan/cache?url=...&elements=[...]\`
Returns \`{ hit: true/false, plan: [...], goal: "...", hitCount: N }\`

\`GET /ui-bridge/control/action-plan/cache/stats\`
Returns \`{ totalEntries, totalHits, maxEntries: 100, ttlSeconds: 3600 }\`

**Personalization pattern (from Skyvern):**
Each action can carry \`userDetailQuery\` ("What email?") and \`userDetailAnswer\` ("test@example.com"). The query is stable across users — only the answer changes. This enables caching the structural plan while personalizing data per user.`,
      estimatedDuration: 2,
    },
    {
      id: "token-budget",
      title: "Token Budget for AI Snapshots",
      content: `When an AI agent takes a page snapshot for planning, large pages can exceed the LLM's context window. The **token budget** feature prunes low-priority elements automatically.

**How to use it:**
\`GET /ui-bridge/ai/snapshot?maxTokens=2000\`

**How pruning works:**
Elements are scored by **region priority** and sorted:

| Region | Priority |
|--------|----------|
| main-content | 100 |
| form | 90 |
| modal | 85 |
| table / card | 80 / 75 |
| toolbar | 70 |
| navigation | 50 |
| sidebar / header | 40 / 30 |
| footer / unknown | 20 / 10 |

Interactive elements get +20, visible elements +10, the focused element +30. The system greedily includes elements in priority order until the budget is exceeded.`,
      tips: [
        "maxTokens=0 means unlimited (same as omitting the parameter)",
        "Element counts are monotonically non-decreasing as maxTokens increases",
        "Token estimation uses ~4 characters per token based on element field lengths",
      ],
      estimatedDuration: 1,
    },
    {
      id: "mcp-tool",
      title: "Using Action Plans from the Agentic Phase",
      content: `During agentic workflow execution, Claude can call action plans through the MCP tool \`sdk_execute_action_plan\`.

**Typical workflow:**
1. Call \`sdk_snapshot\` to see current page state and element IDs
2. Build an action plan using element IDs from the snapshot
3. Execute with \`sdk_execute_action_plan\`
4. Verify with \`sdk_ai_assert\` or another snapshot

**When to use which tool:**

| Scenario | Tool |
|----------|------|
| Single simple action | \`sdk_ai_execute\` |
| Unknown page layout | \`sdk_ai_execute\` |
| 2+ actions in sequence | \`sdk_execute_action_plan\` |
| Element IDs known | \`sdk_execute_action_plan\` |
| Performance matters | \`sdk_execute_action_plan\` |

UI Bridge steps can now be placed directly in the **agentic phase** of workflows (alongside prompt steps), enabling snapshot → plan → execute cycles within a single iteration.`,
      details: `**Goal verification:**
After successful agentic iterations with UI Bridge actions, the workflow executor automatically injects a goal-verification-snapshot step. This catches false-positive completions without requiring explicit assertions.

The injected step carries the agentic outcome's summary as its instruction and is deduplicated across iterations to prevent accumulation.`,
      estimatedDuration: 1,
    },
    {
      id: "api-reference",
      title: "API Reference",
      content: `**Endpoints:**

| Method | Path | Purpose |
|--------|------|---------|
| POST | \`/ui-bridge/control/action-plan\` | Execute a structured action plan |
| POST | \`/ui-bridge/sdk/execute-action-plan\` | Same handler via SDK proxy (for MCP tools) |
| GET | \`/ui-bridge/control/action-plan/cache\` | Look up a cached plan |
| GET | \`/ui-bridge/control/action-plan/cache/stats\` | Cache statistics |
| GET | \`/ui-bridge/ai/snapshot?maxTokens=N\` | Semantic snapshot with token budget |

**Key source files:**
- \`mcp/ui_bridge.rs\` — Endpoint handlers, element resolution, action dispatch
- \`mcp/action_plan_cache.rs\` — LRU cache, fingerprinting, FNV-1a hashing
- \`mcp/ai_session.rs\` — MCP tool documentation for agentic phase
- \`ui-bridge/src/ai/semantic-snapshot.ts\` — Token budget pruning
- \`ui-bridge/src/control/action-executor.ts\` — Autocomplete and dropdown handlers
- \`qontinui-schemas/ts/src/workflow/action-plan.ts\` — TypeScript type definitions`,
      estimatedDuration: 1,
    },
    {
      id: "summary",
      title: "Action Plans Unlocked!",
      content: `You now understand the structured action plan system!

**What You Learned:**
- Action plans execute typed UI actions without natural language interpretation
- Four element resolution strategies: elementId, testId, selector, searchText
- Confidence filtering skips uncertain actions automatically
- Plan caching enables cross-run reuse with element fingerprinting
- Token budget pruning keeps AI context within limits
- The autocomplete action handles complex dropdown interactions

**When to use action plans:**
Whenever an AI agent needs to perform 2+ UI actions in sequence with known element targets. They're faster, cheaper, and more precise than multiple \`sdk_ai_execute\` calls.

**Architecture note:**
This system works with Qontinui's **cooperative** UI Bridge model — apps must integrate the SDK for elements to be registered and targetable. This is by design: it provides the reliability and type safety that non-cooperative approaches (like Skyvern's arbitrary-website scraping) trade away.`,
      estimatedDuration: 1,
    },
  ],
};
