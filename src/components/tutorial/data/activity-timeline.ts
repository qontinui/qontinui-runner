import type { Tutorial } from "@/types/tutorial";

/**
 * Informational tutorial: Activity Timeline & Watchers
 *
 * Explains the screenpipe-inspired capture history system — what it is,
 * how data flows through the pipeline, what the search UI does, and how
 * watchers work as reactive AI agents.
 */
export const activityTimelineTutorial: Tutorial = {
  id: "activity-timeline",
  title: "Activity Timeline & Watchers",
  description:
    "Understand the screenpipe-inspired capture history: how screen content is captured, indexed, searched, and monitored by AI watchers.",
  duration: "12 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  focusPage: "activity-timeline",
  category: "Observe & Debug",
  tags: ["screenpipe", "search", "capture", "watchers", "ai", "featured"],
  learningObjectives: [
    "Understand how screen content is captured from both UI Bridge and black-box desktop apps",
    "Know how content filtering protects sensitive data before storage",
    "Use full-text search to find what was on screen during automation",
    "Understand how watchers monitor the timeline and trigger actions with AI reasoning",
    "Know the complete data pipeline from capture to PostgreSQL to search results",
  ],
  steps: [
    // ======================================================================
    // PART 1: What is the Activity Timeline?
    // ======================================================================
    {
      id: "intro",
      title: "The Activity Timeline",
      content: `The **Activity Timeline** is a searchable history of everything captured on screen during automation — inspired by the open-source project [screenpipe](https://github.com/screenpipe/screenpipe).

Unlike the Runs tab which shows step-by-step execution results, the timeline captures **continuous ambient screen state** — including what happened *between* automation steps. It answers questions like:

- *"What was on screen when step 5 failed?"*
- *"When did the error toast first appear?"*
- *"Was the submit button disabled before or after the navigation?"*`,
      estimatedDuration: 1,
      tips: [
        "The timeline stores text content, not screenshots — this makes it searchable via full-text search",
        "Data is automatically captured in the background while automation runs",
      ],
    },
    {
      id: "capture-sources",
      title: "Three Capture Sources",
      content: `Data flows into the timeline from **three independent sources**, covering both web apps and native desktop apps:

1. **UI Bridge (white-box)** — For JS/TS web apps with the UI Bridge SDK integrated. The \`BackgroundObserver\` captures semantic snapshots every 10 seconds, including element descriptions, form labels, modal content, and page context. This is the richest data source.

2. **Accessibility Tree (black-box)** — For native desktop apps during GUI automation. After each action, the Python bridge walks the OS accessibility tree (Windows UIA, macOS Accessibility, Linux AT-SPI) and extracts structured text from UI elements.

3. **OCR Fallback (black-box)** — When the accessibility tree yields insufficient data (canvas apps, games, custom-rendered UIs), OCR runs on the screenshot as a fallback. This is slower but works on any visual content.`,
      estimatedDuration: 1,
      details: `**UI Bridge captures** use the \`SemanticSnapshotManager\` with change detection via \`SemanticDiffManager\`. Only significant changes (new elements, disappeared elements, text changes, URL changes) trigger a capture, preventing redundant storage.

**Accessibility captures** happen in the \`gui_automation.py\` post-action hook. The tree is walked recursively with a 200-depth guard. Text is extracted from node \`.name\`, \`.value\`, and \`.description\` fields.

**OCR fallback** triggers when fewer than 5 accessibility elements are found, using the configured OCR engine (EasyOCR, PaddleOCR, or Tesseract) via the HAL layer.`,
      tips: [
        "The BackgroundObserver starts 5 seconds after the runner app loads",
        "Each capture source is tagged — you can filter search results by source type",
      ],
    },
    {
      id: "content-pipeline",
      title: "Content Pipeline: Capture to Storage",
      content: `Every capture goes through a **three-stage pipeline** before reaching the database:

**1. Content Filtering** — PII is automatically scrubbed using 7 regex patterns:
- Email addresses become \`[EMAIL]\`
- Credit card numbers become \`[CREDIT_CARD]\`
- SSNs, API keys, bearer tokens, and GitHub tokens are all redacted

Captures from sensitive windows (password managers like 1Password, KeePass, Bitwarden, and incognito/private browsing windows) are **silently skipped entirely**.

**2. Content Hashing** — A SHA-256 hash of the normalized text is computed. If an identical hash was stored within the last 30 seconds, the existing entry's duplicate count is incremented instead of creating a new row.

**3. PostgreSQL Storage** — Text is stored with a GIN full-text search index using \`to_tsvector('english', ...)\`, enabling natural language search with relevance ranking via \`ts_rank\`.`,
      estimatedDuration: 1,
      details: `The content filter (\`ContentFilter\` in \`recording/content_filter.rs\`) uses a \`OnceLock\` singleton pattern. It's applied in two places: the \`insert_activity_entry\` Tauri command (for UI Bridge and direct API calls) and the \`EventForwarder::handle_timeline_capture\` method (for Python bridge events).

The deduplication window of 30 seconds is configured in the \`find_recent_duplicate\` Clorinde query. This prevents the BackgroundObserver (which captures every 10 seconds) from flooding the database with identical snapshots when nothing changes.`,
    },
    // ======================================================================
    // PART 2: Using the Search UI
    // ======================================================================
    {
      id: "search-ui-overview",
      title: "The Search Interface",
      content: `This is the **Activity Timeline search page**. The layout is simple by design — a search bar at the top, optional filters, and a scrollable results area.

The search uses **PostgreSQL full-text search** — you type natural language and it finds matching captured text, ranked by relevance.`,
      estimatedDuration: 1,
      targetElement: {
        selector: '[data-tutorial-id="timeline-page"]',
        highlightType: "border",
        position: "center",
      },
    },
    {
      id: "search-bar",
      title: "Searching the Timeline",
      content: `Type any words you're looking for and press **Enter** or click **Search**.

The query uses PostgreSQL \`plainto_tsquery\` — it tokenizes your words, applies English stemming (so "running" matches "run"), and searches across all captured text. Results are ranked by \`ts_rank\` so the most relevant matches appear first.

Each result shows a **500-character text preview**, the **capture source** (blue for UI Bridge, green for accessibility, amber for OCR), the **app name**, and a **timestamp**.`,
      estimatedDuration: 1,
      targetElement: {
        selector: '[data-tutorial-id="timeline-search-bar"]',
        highlightType: "spotlight",
        position: "bottom",
      },
      tips: [
        "Use specific terms for better results: 'submit button disabled' is better than 'button'",
        "The search runs across ALL captured text — web page content, form labels, modal messages, button text",
      ],
    },
    {
      id: "filters",
      title: "Filtering Results",
      content: `Click the **filter icon** to expand the filter panel. You can narrow results by:

- **App name** — Scope to a specific application (e.g., "Chrome", "VS Code")
- **Source type** — Choose between UI Bridge, Accessibility, or OCR captures

Filters combine with the search query. When filters are active, the search uses the \`search_activity_timeline_filtered\` Tauri command which adds \`WHERE\` clauses to the PostgreSQL query.`,
      estimatedDuration: 1,
      tips: [
        "The stats button (clock icon) shows aggregate counts per source type and capture mode",
        "Stats help you understand how much data has been captured and from which sources",
      ],
    },
    {
      id: "results-anatomy",
      title: "Reading Search Results",
      content: `Each result card contains:

- **Source badge** — Color-coded: blue (\`ui_bridge\`), green (\`accessibility\`), amber (\`ocr\`)
- **App name** — Which application the text was captured from
- **Timestamp** — When the capture occurred (human-readable locale format)
- **Relevance rank** — PostgreSQL \`ts_rank\` score (higher = better match)
- **Text preview** — First 500 characters of the captured text, truncated to 3 lines
- **URL** — If the capture came from a web page, the URL is shown with an external link icon

Results are ordered by relevance rank descending, so the best matches appear first.`,
      estimatedDuration: 1,
      targetElement: {
        selector: '[data-tutorial-id="timeline-results"]',
        highlightType: "border",
        position: "top",
      },
    },
    // ======================================================================
    // PART 3: Watchers — Reactive AI Agents
    // ======================================================================
    {
      id: "watchers-intro",
      title: "Watchers: Reactive AI Agents",
      content: `**Watchers** are the reactive counterpart to the passive timeline. Where the timeline *stores* captures, watchers *act on them*.

Each watcher is a lightweight scheduled agent that:
1. **Runs on a schedule** (every 1 min to every 1 hour)
2. **Queries the timeline** using full-text search with a lookback window
3. **Reasons with AI** — feeds the results into a Claude prompt
4. **Triggers an action** — log, create an observation, or notify

This is the **"if you see X, do Y"** pattern inspired by screenpipe's pipe system.`,
      estimatedDuration: 1,
      tips: [
        "Watchers are stored in PostgreSQL and survive runner restarts",
        "Disabled watchers are skipped during scheduled execution without error",
      ],
    },
    {
      id: "watchers-page",
      title: "The Watchers Management Page",
      content: `The Watchers page (accessible from the sidebar under **Observe**) lets you create, enable/disable, and delete watchers.

Each watcher card shows its **name**, **FTS query** (in monospace), **lookback window** (with clock icon), **last run time**, and any **filters** configured. The toggle switches between enabled (green) and disabled (gray).`,
      estimatedDuration: 1,
      targetElement: {
        selector: '[data-tutorial-id="watchers-header"]',
        highlightType: "spotlight",
        position: "bottom",
      },
    },
    {
      id: "watchers-creation",
      title: "Creating a Watcher",
      content: `Click **New Watcher** to open the creation form. The key fields are:

- **Name** — A descriptive identifier (e.g., "Error Toast Watcher")
- **Timeline Query** — The FTS search terms (e.g., "error failed exception")
- **Schedule Interval** — How often to check (5 minutes is a good default)
- **Lookback Window** — How far back to search (prevents reasoning over stale data)
- **AI Reasoning Prompt** — A template with \`{{results}}\`, \`{{result_count}}\`, and \`{{query}}\` placeholders

The prompt template is pre-filled with a sensible default. Customize it to tell the AI what to look for and what action to recommend.`,
      estimatedDuration: 1,
      details: `**How the lookback window works:** The \`parse_lookback_window()\` function in \`scheduler_service.rs\` converts strings like "15 minutes" into a \`chrono::DateTime\` cutoff. The FTS query runs first (fetching up to 200 results), then results are post-filtered by \`created_at >= cutoff\`, taking at most 50.

**How the prompt is executed:** The formatted prompt is sent via \`reqwest\` to the runner's own \`/prompts/run\` HTTP endpoint, which creates a task run with a single AI session. The AI response is stored in \`last_result_json\` on the watcher record.`,
    },
    {
      id: "watchers-execution",
      title: "How Watchers Execute",
      content: `Watchers are executed by the **SchedulerService** — the same system that runs scheduled workflows and prompts.

The execution pipeline:
1. \`SchedulerService::tick()\` checks for due watcher tasks
2. \`execute_watcher()\` loads the watcher definition from PostgreSQL
3. FTS query runs with lookback window filtering
4. Results are formatted into a numbered list and substituted into the prompt template
5. The prompt is sent to Claude via the runner's prompt execution API
6. The response is recorded and \`last_run_at\` is updated

The **Watcher** step type has a 120-second expected duration to account for AI reasoning time.`,
      estimatedDuration: 1,
      details: `The watcher execution is fully integrated with the scheduler:
- \`ScheduledTaskType::Watcher { watcher_id }\` variant in \`scheduler.rs\`
- \`StepType::Watcher\` variant in \`step_types.rs\` (120s timeout)
- PgDb is passed to the scheduler via \`start_scheduler_service(db, pg_db)\` in main.rs
- Disabled watchers return \`Ok((true, None))\` immediately (no-op)`,
    },
    // ======================================================================
    // PART 4: Architecture Summary
    // ======================================================================
    {
      id: "architecture-summary",
      title: "Architecture at a Glance",
      content: `Here's the complete data flow:

**Capture Layer** (3 sources)
- TypeScript: \`BackgroundObserver\` -> Tauri \`insert_activity_entry\`
- Python: \`gui_automation.py\` -> \`activity_timeline_capture\` event -> \`EventForwarder::handle_timeline_capture\`
- Rust: \`BackgroundCaptureService\` -> \`PgDb::insert_activity_entry\`

**Storage Layer**
- PostgreSQL \`activity_timeline\` table with GIN FTS index
- SHA-256 content-hash deduplication (30-second window)
- \`ContentFilter\` PII scrubbing + sensitive window skipping

**Query Layer** (2 consumers)
- Activity Timeline search UI -> \`search_activity_timeline\` Tauri command
- Watchers -> \`search_timeline_filtered\` + AI reasoning -> action

**Management** (PostgreSQL + Clorinde)
- Tables auto-created via \`ensure_tables()\` on PgDb startup
- All queries type-safe via Clorinde code generation
- 8 Tauri commands for timeline, 6 for watchers`,
      estimatedDuration: 1,
      tips: [
        "All code compiles with cargo check --lib and tsc --noEmit",
        "The feature spans 5 languages: Rust, TypeScript, Python, SQL, and JSON (specs)",
      ],
    },
    {
      id: "whats-next",
      title: "What's Next?",
      content: `You now understand the Activity Timeline and Watchers system. Here are some things to try:

- **Search the timeline** — Run an automation, then search for text you saw on screen
- **Create a watcher** — Set up an "Error Watcher" that monitors for error messages every 5 minutes
- **Check the stats** — Click the clock icon to see how much data has been captured

The timeline becomes more valuable over time as more captures accumulate. Think of it as a searchable memory of everything your automations have seen.`,
      estimatedDuration: 1,
      resources: [
        {
          title: "Screenpipe (inspiration)",
          url: "https://github.com/screenpipe/screenpipe",
        },
      ],
    },
  ],
};
