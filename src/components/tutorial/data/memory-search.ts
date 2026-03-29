import type { Tutorial } from "../../../types/tutorial";

export const memorySearchTutorial: Tutorial = {
  id: "memory-search",
  title: "Unified Memory Search & RRF Fusion",
  description:
    "Understand how the runner searches all memory stores in parallel and fuses results using Reciprocal Rank Fusion.",
  duration: "12 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  focusPage: "memory-search",
  category: "AI Features",
  tags: ["memory", "search", "rrf", "knowledge", "featured"],
  prerequisites: ["getting-started"],
  learningObjectives: [
    "Understand what Reciprocal Rank Fusion (RRF) is and why it outperforms single-source search",
    "Know all 10 memory sources and what data each contains",
    "Learn how 6 retrieval strategies work together to find results",
    "Use source, temporal, and score filters to narrow results",
    "Navigate results efficiently with keyboard shortcuts",
  ],
  steps: [
    // Step 1: Big picture
    {
      id: "intro",
      title: "What is Unified Memory Search?",
      content:
        "The runner accumulates knowledge across every workflow run: **observations**, **findings**, **fixes**, **errors**, **rules**, **timeline captures**, and more. Each lives in a different store (PostgreSQL, SQLite, or the knowledge graph).\n\nUnified Memory Search queries **all stores in parallel** and merges results into a single ranked list using an algorithm called **Reciprocal Rank Fusion** (RRF).",
      targetElement: {
        selector: '[data-tutorial-id="memory-search-header"]',
        highlightType: "spotlight",
        position: "bottom",
      },
      tips: [
        "The 'RRF Fusion' badge next to the title indicates this page uses the fusion algorithm.",
      ],
      details:
        "### Why not just search one store?\n\nA finding about a login bug might exist as:\n- An **observation** captured during the run\n- A **finding** tagged by the reflection engine\n- A **fix** that resolved it\n- An **error event** in the log\n- A **knowledge graph node** linking all of these\n\nSearching only one store would miss the connections. RRF fuses results from all stores so the most relevant item floats to the top regardless of where it was stored.",
    },

    // Step 2: The 10 memory sources
    {
      id: "memory-sources",
      title: "The 10 Memory Sources",
      content:
        "Every result comes from one of **10 sources**, each color-coded:\n\n- **Observation** (blue) — what the system saw during runs\n- **Timeline** (purple) — timestamped screen captures\n- **Knowledge** (green) — learned insights and patterns\n- **Finding** (yellow) — identified issues\n- **Fix** (emerald) — applied solutions\n- **Error** (red) — error events and stack traces\n- **Rule** (cyan) — extracted automation rules\n- **Graph** (orange) — knowledge graph nodes\n- **Workflow** (indigo) — workflow definitions\n- **UI Element** (pink) — UI component data",
      targetElement: {
        selector: '[data-tutorial-id="memory-search-sources"]',
        highlightType: "spotlight",
        position: "bottom",
        scrollIntoView: true,
      },
      tips: [
        "Click a source to filter results to only that type. Multiple selections use AND logic.",
        "When no sources are selected, all 10 are searched.",
      ],
    },

    // Step 3: How RRF works
    {
      id: "rrf-algorithm",
      title: "How Reciprocal Rank Fusion Works",
      content:
        "RRF is elegantly simple. Given multiple ranked lists from different retrieval strategies:\n\n**score(d) = sum( 1 / (k + rank) )** for each strategy that found document *d*\n\nWith **k = 60** (smoothing constant), a document ranked #1 in one list gets `1/61 = 0.016`, ranked #2 gets `1/62 = 0.016`, etc. Scores are then **normalized to 0-1**.\n\nThe key insight: a document found by **multiple strategies** gets boosted because its scores accumulate.",
      details:
        "### Why k = 60?\n\nThe smoothing constant `k` controls how much weight the top ranks get relative to lower ranks. k=60 is the standard value from the original RRF paper (Cormack, Clarke & Butt, 2009). It prevents the top-1 result from dominating and gives lower-ranked results a fair contribution.\n\n### Example\n\nSuppose a finding about 'login timeout' appears:\n- Rank 1 in text search → 1/(60+1) = 0.0164\n- Rank 3 in embedding similarity → 1/(60+3) = 0.0159\n- Total RRF score: 0.0323\n\nAnother finding only appears in text search at rank 2:\n- 1/(60+2) = 0.0161\n\nThe first result wins because it was found by two strategies, even though neither ranked it highest.",
      tips: [
        "This is why 'fused score' differs from 'source score' — fused score reflects multi-strategy agreement.",
      ],
    },

    // Step 4: The 6 retrieval strategies
    {
      id: "retrieval-strategies",
      title: "6 Retrieval Strategies in Parallel",
      content:
        "Each search fans out to **6 parallel retrievers**:\n\n1. **Observation FTS** — PostgreSQL full-text search on observations\n2. **Timeline FTS** — PostgreSQL full-text search on activity timeline\n3. **Knowledge LIKE** — PostgreSQL keyword matching on task knowledge\n4. **SQLite unified** — LIKE search across findings, fixes, errors, rules, workflows, UI elements\n5. **Graph search** — Text match on knowledge graph node labels\n6. **Embedding similarity** — Semantic vector search using 384-dim embeddings\n\nEach returns a ranked list. RRF fuses all six.",
      details:
        "### Strategy tags on results\n\nEach result shows **found_by** tags indicating which strategies discovered it:\n- `FullTextSearch` — matched via PostgreSQL FTS or SQLite LIKE\n- `EmbeddingSimilarity` — matched via cosine similarity on embeddings\n- `GraphTraversal` — found via knowledge graph search\n- `CategoryMatch` — matched by category/type filtering\n\nResults found by multiple strategies generally score higher.",
    },

    // Step 5: Search input
    {
      id: "search-input",
      title: "Searching Memory",
      content:
        "Type a natural language query and press **Enter** or click **Search**. The query is sent to all 6 retrievers simultaneously.\n\nQueries with 2+ characters are accepted. Try searching for concepts like *'login error'*, *'test failure pattern'*, or *'authentication fix'*.",
      targetElement: {
        selector: '[data-tutorial-id="memory-search-input"]',
        highlightType: "pulse",
        position: "bottom",
        allowInteraction: true,
      },
      action: "Try typing a search query",
      tips: [
        "Press / anywhere on the page to focus the search input.",
        "The embedding retriever finds semantically similar results even when exact keywords don't match.",
      ],
    },

    // Step 6: Filters
    {
      id: "filters",
      title: "Filtering Results",
      content:
        "Click the **filter icon** to reveal three filter types:\n\n- **Sources** — toggle specific memory stores on/off\n- **Date range** — restrict to a time window (From/To)\n- **Min score** — hide low-confidence results (slider 0-100%)\n\nFilters apply on the next search. Source filters restrict which retrievers run. Date and score filters apply post-fusion.",
      targetElement: {
        selector: '[data-tutorial-id="memory-search-filters"]',
        highlightType: "spotlight",
        position: "right",
        scrollIntoView: true,
      },
      tips: [
        "Setting min score to 30-50% is a good default to filter noise.",
        "Date filters use RFC3339 format internally — the UI handles the conversion.",
      ],
    },

    // Step 7: Reading results
    {
      id: "reading-results",
      title: "Understanding Results",
      content:
        "Each result shows:\n\n- **Source badge** (colored icon) — which memory store it came from\n- **Title** — the result name or category\n- **Score bar** — the fused RRF score as a percentage\n- **Timestamp** — when the memory was created\n- **Found-by tags** — which retrieval strategies discovered it\n\nClick a result (or press **Enter**) to expand the detail view showing the full snippet, source vs fused scores, and memory type.",
      targetElement: {
        selector: '[data-tutorial-id="memory-search-results"]',
        highlightType: "border",
        position: "left",
        scrollIntoView: true,
      },
      details:
        "### Source Score vs Fused Score\n\n- **Source score** — the raw relevance from the individual retriever (e.g., FTS rank or cosine similarity)\n- **Fused score** — the RRF-combined score after all strategies are merged and normalized\n\nA result can have a low source score but a high fused score if multiple strategies found it independently.",
    },

    // Step 8: Keyboard navigation
    {
      id: "keyboard-shortcuts",
      title: "Keyboard Shortcuts",
      content:
        "Navigate efficiently without touching the mouse:\n\n| Key | Action |\n|-----|--------|\n| **/** | Focus search input |\n| **Arrow Up/Down** | Move selection through results |\n| **Enter** | Expand/collapse selected result |\n| **Escape** | Collapse detail or deselect |\n| **c** | Copy selected result to clipboard |\n\nThe selected result is highlighted with a blue ring. The keyboard hint bar at the bottom of results shows these shortcuts.",
      tips: [
        "Copying formats as '[source] title\\nsnippet' — useful for pasting into prompts or notes.",
      ],
    },

    // Step 9: Architecture overview
    {
      id: "architecture",
      title: "Under the Hood: Data Flow",
      content:
        "The complete search pipeline:\n\n```\nQuery → 6 parallel retrievers\n  ├─ PG FTS (observations)\n  ├─ PG FTS (timeline)\n  ├─ PG LIKE (knowledge)\n  ├─ SQLite LIKE (7 tables)\n  ├─ petgraph search (graph nodes)\n  └─ Embedding cosine similarity\n       ↓\n  RRF fusion (k=60)\n       ↓\n  Temporal & score filters\n       ↓\n  Normalized, ranked results\n```\n\nThe backend caches the **knowledge graph** with a 60-second TTL so graph searches don't rebuild the petgraph on every query. Event-driven invalidation clears the cache when findings, fixes, or rules change.",
      details:
        "### Performance\n\nPostgreSQL FTS and SQLite LIKE run in parallel via `tokio::join!`. The embedding retriever first checks if the embedding API is available (it may be offline) and gracefully skips if unavailable.\n\nThe graph retriever uses a pre-built in-memory petgraph with 12 node types and 11 edge types. A generation counter triggers rebuild when underlying data changes.\n\n### Memory consolidation\n\nA background scheduler runs every 10 minutes to consolidate raw observations into higher-level 'mental models' using TF-IDF content similarity clustering. Observations that decay below importance thresholds are archived.",
    },

    // Step 10: Practical tips
    {
      id: "practical-tips",
      title: "Tips for Effective Searching",
      content:
        "**Start broad, then narrow:**\n- Search with general terms first, then add source filters\n- Use min score 30%+ to cut noise on broad queries\n\n**Leverage embeddings:**\n- Semantic search finds related concepts even without exact keyword matches\n- 'authentication problem' can find results about 'login failure'\n\n**Cross-run learning:**\n- Knowledge and patterns persist across workflow runs\n- Search for past solutions before debugging from scratch\n\n**Use the graph source:**\n- Graph nodes capture causal relationships between errors, findings, and fixes\n- Filter to 'graph' source to find connected patterns",
      tips: [
        "The memory search is also available as an MCP tool — workflow agents can call 'query_memory' to search accumulated knowledge during execution.",
        "The orchestrator automatically enriches AI prompts with top unified memory results for the current task.",
      ],
    },

    // Step 11: Summary
    {
      id: "summary",
      title: "What You Learned",
      content:
        "You now understand:\n\n- **RRF fusion** merges 6 retrieval strategies into a single ranked list\n- **10 memory sources** cover observations, findings, fixes, errors, rules, timeline, knowledge, graph, workflows, and UI elements\n- **Fused scores** reflect multi-strategy agreement, not just single-source relevance\n- **Filters** narrow by source, time range, and minimum confidence\n- **Keyboard shortcuts** enable fast navigation (/ ↑↓ Enter c)\n- **Architecture**: parallel fan-out, 60s graph cache, background consolidation\n\nExplore the **Knowledge Explorer** tab to browse the graph visually, or use **Activity Timeline** for chronological capture history.",
      resources: [
        {
          title: "RRF Paper (Cormack et al., 2009)",
          url: "https://plg.uwaterloo.ca/~gvcormac/cormacksigir09-rrf.pdf",
          type: "article",
        },
      ],
    },
  ],
};
