import type { Tutorial } from "../../../types/tutorial";

/**
 * Informational tutorial explaining the Temporal Memory Retrieval system —
 * how observations track when knowledge was true, detect superseded facts,
 * and support time-as-a-first-class-query-dimension.
 *
 * Inspired by Zep/Graphiti (validity windows), MAGMA (temporal graph),
 * and Hindsight (time-decay weighting in retrieval scoring).
 */
export const temporalMemoryRetrievalTutorial: Tutorial = {
  id: "temporal-memory-retrieval",
  title: "Temporal Memory Retrieval: When Was Knowledge True?",
  description:
    "Understand how the temporal validity system tracks when observations were true, detects superseded facts, preserves revision history, and uses time-decay scoring to rank memories by freshness and relevance.",
  duration: "14 minutes",
  difficulty: "advanced",
  mode: "contextual",
  focusPage: "help",
  category: "AI Features",
  tags: [
    "featured",
    "temporal",
    "memory",
    "observations",
    "validity",
    "supersession",
    "history",
    "scoring",
    "retrieval",
  ],
  prerequisites: ["observation-memory"],
  learningObjectives: [
    "Understand temporal validity windows (valid_from, valid_until, superseded_by)",
    "Learn how the history snapshotting mechanism preserves every version of a topic-keyed observation",
    "Understand the temporal scoring formula: recency decay, currency boost, supersession penalty",
    "Know how temporal scoring integrates with RRF fusion in the unified memory search",
    "Use temporal queries: date-range filtering, point-in-time snapshots, and trend analysis",
    "Navigate the Observation Browser with temporal controls",
  ],

  steps: [
    // -----------------------------------------------------------------------
    // Step 1: Why Temporal Memory?
    // -----------------------------------------------------------------------
    {
      id: "why-temporal",
      title: "The Problem: Memory Without Time",
      content: `Standard observation storage knows **what** knowledge exists, but not **when** it was true.

When an architecture decision changes, the old version is overwritten. When a bug is fixed, the observation that described the bug still appears alongside current knowledge. There's no way to ask:

- *"What did we know about auth last week?"*
- *"How has our database strategy evolved over the last month?"*
- *"Which observations have been replaced by newer understanding?"*

**Temporal memory retrieval** solves this by making time a first-class query dimension — every observation tracks when it became valid, when it expired, and what replaced it.`,
      estimatedDuration: 1.5,
      tips: [
        "Inspired by Zep/Graphiti (validity windows), MAGMA (temporal graph), and Hindsight (time-decay scoring)",
      ],
    },

    // -----------------------------------------------------------------------
    // Step 2: Validity Windows
    // -----------------------------------------------------------------------
    {
      id: "validity-windows",
      title: "Validity Windows: valid_from / valid_until",
      content: `Every observation now has a **validity window** — the time period during which it represents current knowledge.

| Column | Meaning |
|--------|---------|
| \`valid_from\` | When this version of the observation became the truth |
| \`valid_until\` | When it stopped being current (\`NULL\` = still valid) |
| \`superseded_by\` | ID of the observation that replaced this one |

**A \`NULL\` valid_until means the observation is current.** Once it's superseded or manually expired, \`valid_until\` gets a timestamp.

This simple model enables point-in-time queries: "Show me everything that was true on March 15th" becomes a straightforward SQL filter.`,
      estimatedDuration: 1.5,
      details: `**SQL for point-in-time query:**
\`\`\`sql
SELECT * FROM observations
WHERE valid_from <= '2026-03-15'
  AND (valid_until IS NULL OR valid_until >= '2026-03-15')
\`\`\`

This returns observations that were valid at that exact moment — started before the query time and either still current or expired after it.`,
    },

    // -----------------------------------------------------------------------
    // Step 3: History Snapshotting
    // -----------------------------------------------------------------------
    {
      id: "history-snapshotting",
      title: "History Snapshotting: Never Lose a Version",
      content: `When a topic-keyed observation is updated via upsert, the **previous version is automatically preserved** in the \`observation_history\` table.

**The atomic snapshot + upsert flow:**
1. Begin transaction
2. Copy current title, content, and hash into \`observation_history\` with \`valid_until = NOW()\`
3. Update the observation: new content, \`valid_from = NOW()\`, increment \`revision_count\`
4. Commit transaction

This means every version of every topic-keyed observation is preserved with its exact validity window. You can trace the full evolution of "architecture/auth-model" from its first creation through every revision.`,
      estimatedDuration: 1.5,
      tips: [
        "The transaction prevents concurrent upserts from losing history entries",
        "Non-topic-keyed observations don't get history — they're immutable once created",
        "The history table stores a 500-char content preview, not the full content",
      ],
      details: `**observation_history schema:**
\`\`\`
observation_id  → FK to current observation
title, content  → Snapshot of the old version
content_hash    → For dedup verification
valid_from      → When this version became valid
valid_until     → When it was replaced (always set)
revision_number → Sequential counter (1, 2, 3...)
\`\`\``,
    },

    // -----------------------------------------------------------------------
    // Step 4: Supersession
    // -----------------------------------------------------------------------
    {
      id: "supersession",
      title: "Supersession: Explicit Knowledge Replacement",
      content: `Sometimes one observation **replaces** another entirely — not just a revision of the same topic, but a new observation that makes an old one obsolete.

**Supersession** is an explicit action (not automatic):
- The old observation gets \`superseded_by = new_id\` and \`valid_until = NOW()\`
- The old observation is **not deleted** — it remains as historical context
- Search results still include superseded observations, but they're **deprioritized**

This happens during:
- **Memory consolidation**: when raw observations are synthesized into mental models
- **Manual API calls**: \`POST /observations/{id}/supersede\`
- **Frontend actions**: the Observation Browser's supersede button`,
      estimatedDuration: 1,
      tips: [
        "Superseded observations appear dimmed with a strikethrough in the browser",
        "The 'Show Superseded' toggle lets you hide them entirely",
      ],
    },

    // -----------------------------------------------------------------------
    // Step 5: Temporal Scoring Formula
    // -----------------------------------------------------------------------
    {
      id: "temporal-scoring",
      title: "Temporal Scoring: The Three-Factor Formula",
      content: `When observations are retrieved for AI prompts or memory search, they're ranked by a **temporal relevance score** that combines three factors:

**1. Recency Decay** — exponential decay with ~23-day half-life:
\`score = e^(-0.03 × age_days)\`

**2. Currency Boost** — current observations (valid_until = NULL) get 1.0; expired ones get 0.3

**3. Supersession Penalty** — non-superseded observations get 1.0; superseded ones get 0.2

**Final score = recency × currency × supersession**

A 1-day-old current observation scores ~0.97. A 30-day-old superseded observation scores ~0.024 — effectively buried in results but still findable.`,
      estimatedDuration: 2,
      details: `**Score examples at different ages:**

| Age | Current | Expired | Superseded |
|-----|---------|---------|------------|
| 1 day | 0.97 | 0.29 | 0.19 |
| 7 days | 0.81 | 0.24 | 0.16 |
| 23 days | 0.50 | 0.15 | 0.10 |
| 60 days | 0.17 | 0.05 | 0.03 |
| 90 days | 0.07 | 0.02 | 0.01 |

The 23-day half-life was chosen to balance freshness against institutional knowledge — important facts remain relevant for weeks, but stale observations gradually fade from prominence.`,
      tips: [
        "The decay constant 0.03 gives a half-life of ln(2)/0.03 ≈ 23 days",
        "Mental models (consolidated knowledge) use importance-based scoring instead, with a 20% boost",
      ],
    },

    // -----------------------------------------------------------------------
    // Step 6: Integration with Unified Memory Search
    // -----------------------------------------------------------------------
    {
      id: "rrf-integration",
      title: "Temporal Scoring in Unified Memory Search",
      content: `The unified memory search (\`/memory/search\`) queries **all** memory stores in parallel and fuses results using **Reciprocal Rank Fusion** (RRF).

For observations specifically, the per-source score is:
\`source_score = FTS_rank × temporal_score\`

This means a highly relevant but old/superseded observation gets downranked compared to a moderately relevant but fresh observation.

**The fusion pipeline:**
1. Query observations, timeline, task_knowledge, SQLite entities, graph nodes — in parallel
2. Each source normalizes scores to 0.0–1.0
3. Observations multiply FTS rank by temporal score
4. RRF fuses all sources: \`fused(d) = Σ 1/(k + rank_s(d))\` with k=60
5. Apply temporal filters (from/to) and score thresholds
6. Return top N results`,
      estimatedDuration: 1.5,
      details: `**Why multiply FTS × temporal?**

Additive combination (\`FTS + temporal\`) would let a highly time-relevant but textually irrelevant result score well. Multiplication ensures both dimensions must be strong — a perfect temporal score can't compensate for zero text relevance, and vice versa.

**Why RRF over weighted sum?**

RRF is rank-based, not score-based. This means it doesn't need score calibration across heterogeneous sources (PG FTS scores vs SQLite relevance vs graph position). It naturally handles the "different scales" problem.`,
    },

    // -----------------------------------------------------------------------
    // Step 7: Temporal Query Types
    // -----------------------------------------------------------------------
    {
      id: "temporal-queries",
      title: "Three Types of Temporal Queries",
      content: `The temporal search endpoint supports three query patterns:

**1. Date Range** (\`from\` / \`to\`)
Find observations whose \`valid_from\` falls within the range. Answers: *"What new knowledge was captured last week?"*

**2. Point-in-Time Snapshot** (\`as_of\`)
Find observations that were valid at a specific moment. Answers: *"What did we know about auth on March 15th?"*

**3. Combined with FTS** (\`q\` + temporal filters)
Full-text search narrowed by time window. Answers: *"What did we learn about performance in the last 30 days?"*

All three can be combined: \`q=auth&from=2026-03-01&to=2026-03-27\``,
      estimatedDuration: 1,
      tips: [
        "The as_of parameter triggers snapshot mode — different SQL logic than date range",
        "Without any temporal filters, the endpoint returns all recent observations (default limit 20)",
        "Results are ordered: current observations first, then by valid_from descending",
      ],
    },

    // -----------------------------------------------------------------------
    // Step 8: Aggregation Queries
    // -----------------------------------------------------------------------
    {
      id: "aggregation-queries",
      title: "Trend Analysis & Most-Revised Topics",
      content: `Beyond search, the temporal system supports analytical queries:

**Weekly Trends** (\`GET /observations/trends?weeks=8\`)
Returns observation counts grouped by type and week. Shows patterns like "we captured many bugfix observations this sprint" or "architecture observations peaked when we redesigned the auth system."

**Most-Revised Topics** (\`GET /observations/most-revised?from=...&to=...\`)
Returns topic-keyed observations with the highest revision counts in a period. Identifies knowledge that's actively evolving — useful for finding areas of rapid change or instability.

**Revision History** (\`GET /observations/{id}/history\`)
Returns all prior versions of a specific observation with their validity windows. Shows the full evolution of a piece of knowledge over time.`,
      estimatedDuration: 1,
    },

    // -----------------------------------------------------------------------
    // Step 9: The Observation Browser
    // -----------------------------------------------------------------------
    {
      id: "observation-browser",
      title: "The Observation Browser UI",
      content: `The web app includes a full **Observation Browser** at \`/observations\` with temporal controls:

**Toolbar:**
- **Search**: Full-text search across all observations
- **Date Range**: Filter by when observations were created
- **As-Of Date**: Switch to snapshot mode — see knowledge as it existed at a past moment
- **Superseded Toggle**: Show/hide expired and superseded observations
- **Trends Toggle**: Show weekly observation trend chart

**Results List:**
- Type badges (architecture, bugfix, pattern, etc.) with color coding
- Temporal badges: "current" (green), "expired" (dimmed), "superseded" (strikethrough)
- Revision count indicator for multiply-revised topics

**Detail Panel:**
- Temporal validity card (valid_from, valid_until, superseded_by)
- Expandable revision history timeline with all prior versions
- Topic key display for upsert-tracked observations`,
      estimatedDuration: 1.5,
    },

    // -----------------------------------------------------------------------
    // Step 10: Data Flow End-to-End
    // -----------------------------------------------------------------------
    {
      id: "end-to-end-flow",
      title: "End-to-End: From Observation to Retrieval",
      content: `Let's trace how temporal memory works in practice:

**1. Creation**: A workflow discovers "auth uses JWT with 15min expiry" → saved with topic_key \`architecture/auth-model\`, \`valid_from = NOW()\`

**2. Evolution**: A week later, the decision changes to 30min expiry → old version snapshotted to history, new version gets \`valid_from = NOW()\`, \`revision_count = 2\`

**3. Retrieval**: AI prompt asks about auth → temporal search finds the observation, temporal score weights it by age (fresh = high score)

**4. Supersession**: Later, JWT is replaced by session tokens → new observation created, old one superseded (\`valid_until = NOW()\`, \`superseded_by = new_id\`)

**5. Historical Query**: Developer asks "what was our auth strategy last month?" → point-in-time snapshot shows the JWT observation as it existed then, even though it's now superseded`,
      estimatedDuration: 1.5,
    },

    // -----------------------------------------------------------------------
    // Step 11: The Backfill & Migration Strategy
    // -----------------------------------------------------------------------
    {
      id: "migration-strategy",
      title: "Migration: Handling Existing Data",
      content: `The temporal system was added to an existing observation store. Here's how migration works:

**New columns** (\`valid_from\`, \`valid_until\`, \`superseded_by\`) were added with safe defaults:
- \`valid_from\` defaults to \`NOW()\` for the ALTER TABLE
- A backfill UPDATE sets \`valid_from = created_at\` for existing rows (where valid_from ≈ updated_at, indicating it was set by the default rather than explicitly)
- \`valid_until\` defaults to \`NULL\` (all existing observations are treated as current)

**The \`observation_history\` table** starts empty — only future upserts create history entries. Existing revision counts are preserved but prior versions aren't retroactively captured.

This "forward-looking" approach avoids data reconstruction risks while ensuring all future changes are tracked.`,
      estimatedDuration: 1,
      tips: [
        "The ensure_tables() DDL is idempotent — safe to run on every startup",
        "The backfill condition (valid_from = updated_at AND created_at < updated_at) prevents re-backfilling already-correct rows",
      ],
    },

    // -----------------------------------------------------------------------
    // Step 12: Mental Model
    // -----------------------------------------------------------------------
    {
      id: "mental-model",
      title: "Mental Model: Time as a Query Dimension",
      content: `Here's the key mental model for temporal memory retrieval:

**Every observation is a fact with a validity window.** Not all facts are forever — code evolves, decisions change, bugs get fixed. The temporal system acknowledges this by tracking *when* knowledge was true, not just *what* was known.

**Three time-aware operations:**
- **Filter by time**: "What was true during this period?" (date range + as-of)
- **Rank by freshness**: "Which knowledge is most relevant right now?" (temporal scoring)
- **Track evolution**: "How has this knowledge changed?" (revision history)

**Two types of knowledge expiration:**
- **Revision**: Same topic, new content → old version moves to history
- **Supersession**: Different observation replaces this one → soft expiration with penalty

The result: an AI that not only remembers what it learned, but understands the **currency** and **provenance** of that knowledge — preferring fresh, current observations while still being able to answer historical questions.`,
      estimatedDuration: 1.5,
    },
  ],
};
