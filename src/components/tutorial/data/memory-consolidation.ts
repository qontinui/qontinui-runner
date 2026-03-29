import type { Tutorial } from "@/types/tutorial";

/**
 * Informational tutorial explaining the memory consolidation system:
 * importance scoring, Ebbinghaus decay, mental model synthesis, and the
 * 4-phase consolidation pipeline.
 */
export const memoryConsolidationTutorial: Tutorial = {
  id: "memory-consolidation",
  title: "Memory Consolidation & Decay",
  description:
    "Understand how Qontinui synthesizes raw observations into long-lived mental models using importance-weighted decay and periodic LLM-driven consolidation.",
  duration: "12 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  focusPage: "help",
  category: "Architecture",
  tags: ["memory", "consolidation", "decay", "mental-models", "informational"],
  prerequisites: ["observation-memory"],
  learningObjectives: [
    "Understand the 3-tier memory hierarchy: raw observations, high-importance observations, and mental models",
    "Learn how importance scores are computed from type, duplicates, revisions, access, and task linkage",
    "Understand Ebbinghaus-inspired decay curves and differential decay rates",
    "Trace the 4-phase consolidation pipeline: Orient, Gather, Consolidate, Prune",
    "Know how mental models surface during memory retrieval with score boosts",
    "Use the Memory Health panel to monitor and trigger consolidation",
  ],

  steps: [
    // Step 1: Big picture
    {
      id: "overview",
      title: "Why Memory Consolidation?",
      content: `Over time, the runner accumulates hundreds of **observations** from task runs — patterns, decisions, bugfixes, architecture notes. Without consolidation:

- **Signal drowns in noise** — important insights get buried under routine observations
- **Retrieval slows** — searching 500 raw observations is slower than searching 50 mental models
- **Context windows waste tokens** — injecting raw observations into prompts is inefficient

Memory consolidation solves this by **promoting clusters of related observations into concise mental models**, while letting stale observations naturally decay.`,
      tips: [
        "Inspired by FadeMem (NeurIPS), Hindsight, and Claude Code's AutoDream consolidation",
        "Mental models are stored as observations with is_mental_model=true — same table, same search index",
      ],
      estimatedDuration: 1,
    },

    // Step 2: The Memory Health panel
    {
      id: "health-panel",
      title: "The Memory Health Panel",
      content: `This panel gives you a real-time view of the memory system's health. It shows four key metrics and a log of recent consolidation runs.

The panel lives at the top of the **Memory** tab, above the observation browser.`,
      targetElement: {
        selector: '[data-tutorial-id="memory-health-panel"]',
        highlightType: "spotlight",
        position: "bottom",
        scrollIntoView: true,
      },
      estimatedDuration: 0.5,
    },

    // Step 3: Health stats
    {
      id: "health-stats",
      title: "Memory Health Metrics",
      content: `The four stat cards show:

- **Observations** — Total non-consolidated raw observations in the database
- **Mental Models** — Synthesized high-level summaries created by consolidation
- **Decay Queue** — Observations whose retention has dropped below 5% (candidates for archival)
- **Last Consolidation** — When the consolidation pipeline last ran successfully

A healthy system has a growing mental model count and a small decay queue.`,
      targetElement: {
        selector: '[data-tutorial-id="memory-health-stats"]',
        highlightType: "border",
        position: "bottom",
        scrollIntoView: true,
      },
      tips: [
        "The decay queue uses the formula: retention = importance * e^(-decay_rate * days_since_access)",
        "Observations below 5% retention are soft-deleted, not permanently removed",
      ],
      estimatedDuration: 1,
    },

    // Step 4: Importance scoring
    {
      id: "importance-scoring",
      title: "Importance Scoring",
      content: `Every observation gets an **importance score** (0.0 to 1.0) that determines how long it survives and how prominently it surfaces in queries.

The score combines:

| Factor | Boost |
|--------|-------|
| **Base type** | architecture=0.8, decision=0.7, bugfix=0.6, pattern/learning=0.5, discovery=0.4 |
| **Duplicates** | +0.05 per duplicate (cap +0.2) — confirmed observations are more important |
| **Revisions** | +0.05 per revision beyond 1 (cap +0.2) — evolving knowledge matters |
| **Access count** | +0.02 per retrieval (cap +0.1) — frequently recalled = important |
| **Task-linked** | +0.1 if tied to a specific task run |`,
      details: `### Example Calculation

An "architecture" observation (base 0.8) that has been duplicated 3 times (+0.15), revised twice (+0.05), accessed 4 times (+0.08), and is linked to a task (+0.1):

\`\`\`
importance = min(0.8 + 0.15 + 0.05 + 0.08 + 0.1, 1.0) = 1.0
\`\`\`

The score is capped at 1.0. This observation will decay very slowly and surface prominently in queries.

Source: \`src-tauri/src/memory/importance.rs\``,
      estimatedDuration: 1.5,
    },

    // Step 5: Decay curves
    {
      id: "decay-curves",
      title: "Ebbinghaus Decay Curves",
      content: `Observations fade over time using an **exponential decay** formula inspired by Ebbinghaus's forgetting curve:

\`\`\`
retention = importance * e^(-decay_rate * days_since_access)
\`\`\`

**Differential decay rates** ensure valuable knowledge survives longer:

| Category | Decay Rate | Half-life at importance=0.8 |
|----------|------------|----------------------------|
| Mental models | 0.02/day | ~139 days |
| High-importance (>=0.8) | 0.05/day | ~55 days |
| Regular observations | 0.1/day | ~23 days |

When retention drops below **0.05** (the archive threshold), the observation is soft-deleted.`,
      tips: [
        "Accessing an observation resets its last_accessed_at, effectively refreshing its decay clock",
        "Mental models decay 5x slower than regular observations — consolidated knowledge is preserved",
      ],
      details: `### Decay in Practice

A regular observation with importance 0.5 and decay rate 0.1:
- **Day 0**: retention = 0.50 (fresh)
- **Day 7**: retention = 0.50 * e^(-0.7) = 0.25
- **Day 14**: retention = 0.50 * e^(-1.4) = 0.12
- **Day 23**: retention = 0.50 * e^(-2.3) = 0.05 (archived)

A mental model with importance 0.8 and decay rate 0.02:
- **Day 0**: retention = 0.80
- **Day 30**: retention = 0.80 * e^(-0.6) = 0.44
- **Day 90**: retention = 0.80 * e^(-1.8) = 0.13
- **Day 139**: retention = 0.80 * e^(-2.78) = 0.05 (archived)

Source: \`src-tauri/src/memory/decay.rs\``,
      estimatedDuration: 1.5,
    },

    // Step 6: The 4-phase pipeline
    {
      id: "consolidation-pipeline",
      title: "The 4-Phase Consolidation Pipeline",
      content: `Consolidation runs periodically (every 6 hours by default) and follows four phases:

**A. Orient** — Group related observations by topic prefix, observation type, or TF-IDF content similarity

**B. Gather** — For each group of 3+ observations, extract key themes and build an LLM prompt

**C. Consolidate** — A lightweight (Haiku-class) LLM synthesizes each group into a single mental model

**D. Prune** — Create the mental model, reduce source observation importance by 50%, and archive decayed observations`,
      tips: [
        "The pipeline uses a 6-hour cooldown to prevent excessive consolidation",
        "You can trigger consolidation manually from the Consolidate button",
      ],
      estimatedDuration: 1,
    },

    // Step 7: Grouping strategies
    {
      id: "grouping-strategies",
      title: "Phase A: Grouping Strategies",
      content: `The Orient phase uses three strategies to find related observations, applied in priority order:

1. **Topic prefix** — Observations with the same topic-key prefix (e.g., \`auth/token\`, \`auth/session\`, \`auth/middleware\` all group under \`auth\`)

2. **Same type** — Unassigned observations of the same type (e.g., all \`bugfix\` observations)

3. **TF-IDF cosine similarity** — For remaining observations, compute term-frequency/inverse-document-frequency vectors and cluster those with cosine similarity >= 0.25

Each observation is assigned to at most one group. Groups must have **3+ members** to trigger consolidation.`,
      details: `### TF-IDF Clustering Details

The TF-IDF approach:
1. Tokenize title + content, remove stop words (50+ common words filtered)
2. Compute term frequency (normalized by document length)
3. Compute inverse document frequency across all remaining observations
4. Multiply TF * IDF to get weighted term vectors
5. Compute pairwise cosine similarity between all observation vectors
6. Greedy single-linkage clustering: seed a cluster, add all observations above 0.25 similarity

This catches thematic clusters that don't share topic keys or types — e.g., several observations about "database connection timeout" patterns across different types.

Source: \`src-tauri/src/memory/consolidation.rs\``,
      estimatedDuration: 1.5,
    },

    // Step 8: LLM consolidation
    {
      id: "llm-synthesis",
      title: "Phase C: LLM Synthesis",
      content: `For each group, the pipeline builds a prompt containing all source observations (sorted by importance) and asks a lightweight LLM to:

1. **Identify the core insight** connecting the observations
2. **Note contradictions** or evolution over time
3. **Produce a mental model** — a 2-4 sentence actionable summary

The LLM returns structured JSON with:
- \`title\` and \`content\` for the mental model
- \`keywords\` for future retrieval
- \`supersedes\` — which source observation IDs are fully captured
- \`contradictions\` — any noted conflicts between sources`,
      tips: [
        "Uses a Haiku-class model to minimize cost — consolidation is a background housekeeping task",
        "Temperature is set to 0.3 for more deterministic, structured output",
        "The model override is configurable in runner settings",
      ],
      estimatedDuration: 1,
    },

    // Step 9: Mental model promotion
    {
      id: "mental-model-promotion",
      title: "Phase D: Mental Model Creation",
      content: `After LLM synthesis, the pipeline:

1. **Creates the mental model** as a new observation with \`is_mental_model=true\` and importance >= 0.7
2. **Records provenance** — the \`consolidated_from\` field stores the IDs of all source observations
3. **Reduces source importance** — superseded observations have their importance cut by 50%
4. **Runs the decay pass** — archives all observations (and expired mental models) below the 0.05 retention threshold
5. **Picks the broadest scope** — if sources span project and global scope, the mental model inherits global`,
      estimatedDuration: 1,
    },

    // Step 10: Retrieval enhancement
    {
      id: "retrieval-enhancement",
      title: "How Mental Models Surface",
      content: `Mental models get preferential treatment during memory retrieval:

- **Deduplication** — Raw observations that were consolidated into a mental model are filtered out of search results (no double-counting)
- **Score boost** — Mental models receive a **20% importance boost** in retrieval scoring
- **Word-level matching** — Mental models match queries by word overlap (at least half the query words must appear), avoiding exact-substring limitations
- **Access tracking** — Every time an observation or mental model is retrieved, its \`access_count\` increments and \`last_accessed_at\` resets — refreshing its decay clock

This means frequently-used mental models become self-reinforcing: the more they're retrieved, the longer they survive.`,
      estimatedDuration: 1,
    },

    // Step 11: Consolidate button
    {
      id: "consolidate-button",
      title: "Manual Consolidation",
      content: `Click this button to trigger consolidation manually instead of waiting for the automatic 6-hour interval.

The button enforces the same cooldown — if consolidation ran recently, you'll see a "Consolidation is on cooldown" message.

After a successful run, a green banner shows how many mental models were created, observations merged, and observations archived.`,
      targetElement: {
        selector: '[data-tutorial-id="memory-consolidate-btn"]',
        highlightType: "pulse",
        position: "bottom",
        scrollIntoView: true,
      },
      estimatedDuration: 0.5,
    },

    // Step 12: Consolidation log
    {
      id: "consolidation-log",
      title: "Consolidation Run History",
      content: `The log shows recent consolidation runs with key metrics:

- **Models created** — How many mental models were synthesized
- **Scanned** — Total observations examined
- **Archived** — Observations that decayed below the retention threshold

Failed runs show the error message. A healthy system shows regular successful runs with a gradually growing mental model count.`,
      targetElement: {
        selector: '[data-tutorial-id="memory-consolidation-log"]',
        highlightType: "border",
        position: "top",
        scrollIntoView: true,
      },
      estimatedDuration: 0.5,
    },

    // Step 13: Configuration
    {
      id: "configuration",
      title: "Tuning Consolidation",
      content: `Consolidation parameters are configurable in the runner's \`settings.json\` under \`memory_consolidation\`:

| Setting | Default | Purpose |
|---------|---------|---------|
| \`min_group_size\` | 3 | Minimum observations needed to form a group |
| \`cooldown_hours\` | 6.0 | Hours between consolidation runs |
| \`archive_threshold\` | 0.05 | Retention level below which observations are archived |
| \`max_observations\` | 500 | Maximum observations scanned per run |
| \`model_override\` | null | LLM model for consolidation (Haiku-class recommended) |
| \`provider_override\` | null | LLM provider for consolidation |

Lower \`min_group_size\` to consolidate more aggressively, or raise \`cooldown_hours\` to reduce frequency.`,
      estimatedDuration: 1,
    },
  ],
};
