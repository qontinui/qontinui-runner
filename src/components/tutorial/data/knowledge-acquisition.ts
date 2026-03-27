/**
 * Knowledge Acquisition Tutorial (Informational)
 *
 * Explains the multi-provider external knowledge system — how the runner
 * searches the web, vulnerability databases, and API docs to augment its
 * understanding during workflow execution. Covers the tiered search architecture,
 * knowledge flywheel, provider ecosystem, observability stats, and security
 * protections (prompt injection scanning).
 */

import type { Tutorial } from "../../../types/tutorial";

export const knowledgeAcquisitionTutorial: Tutorial = {
  id: "knowledge-acquisition",
  title: "Knowledge Acquisition System",
  description:
    "How the runner searches external sources (web, vulnerability databases, API docs) to fill knowledge gaps during workflow execution, and how results are cached via the knowledge flywheel.",
  duration: "10 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  focusPage: "knowledge-explorer",
  category: "Architecture",
  tags: [
    "knowledge-acquisition",
    "search",
    "architecture",
    "vulnerability",
    "flywheel",
    "featured",
  ],
  prerequisites: ["getting-started"],
  learningObjectives: [
    "Understand why the runner needs external knowledge",
    "Know the 4-tier search architecture and escalation strategy",
    "Understand the knowledge flywheel (search → embed → store → retrieve)",
    "Know which providers are available and what they're best at",
    "Read the observability stats to gauge system health",
    "Understand the prompt injection protections on ingested content",
  ],
  steps: [
    {
      id: "intro",
      title: "Why Knowledge Acquisition?",
      content: `The runner's AI workflows often encounter **unfamiliar errors, unknown APIs, or security vulnerabilities** that aren't in their training data. Rather than failing or hallucinating, the runner can **search external sources** for real, current information.

**The Knowledge Acquisition system** is a multi-provider search layer that:
- Searches DuckDuckGo, Tavily, Perplexity, Sploitus, and OSV.dev
- Summarizes and embeds results for future retrieval
- Automatically enriches security findings with CVE/exploit data
- Provides the fix agent with error-resolution context

It's the runner's way of **learning from the internet** — not just from past runs.`,
      estimatedDuration: 1,
      tips: [
        "The system works without any API keys — DuckDuckGo, Sploitus, and OSV.dev are always free",
        "Add TAVILY_API_KEY and PERPLEXITY_API_KEY env vars to unlock paid providers",
      ],
    },
    {
      id: "tiered-search",
      title: "The 4-Tier Search Architecture",
      content: `Every search follows a **tiered escalation strategy** that minimizes latency and API costs:

| Tier | Source | When it runs | Cost |
|------|--------|-------------|------|
| **0** | Local Cache | Always first | Free (SQLite/PG) |
| **1** | UI Bridge | Connected app | Free (local) |
| **2** | Web Search | Cache miss | Free → Paid |
| **3** | Ingest | After results | Free |

**Tier 0** checks if the answer already exists in \`task_knowledge\` via embedding similarity (cosine ≥ 0.5). If a match is found, the search returns instantly — **no external calls**.

**Tier 2** escalates through providers: DuckDuckGo first (free, fast), then Tavily if < 2 results (paid, deeper), then Perplexity as last resort (premium, synthesized).

**Tier 3** automatically stores results back into \`task_knowledge\`, closing the **knowledge flywheel**.`,
      estimatedDuration: 2,
      tips: [
        "The cache hit rate (visible in Stats tab) shows what % of searches are served from local cache",
        "A high cache hit rate means the flywheel is working — the system learns from every search",
      ],
      details: `### The Flywheel In Detail

1. A search query arrives (e.g., from the fix agent encountering an unfamiliar error)
2. Tier 0: The query is embedded via MiniLM-L6-v2 (384 dimensions) and compared against existing \`task_knowledge\` entries
3. If similarity ≥ 0.5, return cached results immediately (cache hit)
4. If no match, proceed to Tier 2 web search
5. Results are summarized (if > 3000 chars, via Claude Haiku)
6. Each result is embedded and checked for duplicates (cosine > 0.85 = skip)
7. New results are inserted into \`task_knowledge\` with category \`external_knowledge\`
8. Next time someone searches for a similar topic, Tier 0 serves the cached answer

**Budget enforcement**: Each search session is capped at 5 actions, 80KB total content, 50KB per result, and 30s per provider call.`,
    },
    {
      id: "providers",
      title: "The Provider Ecosystem",
      content: `Six search providers, each with different strengths:

| Provider | Type | Best For |
|----------|------|----------|
| **DuckDuckGo** | Web search | Quick error lookups, general questions |
| **Tavily** | Deep search | API docs, full page content, AI answers |
| **Perplexity** | AI synthesis | Complex multi-source research |
| **Sploitus** | Exploit DB | CVE lookup, exploit PoC code |
| **OSV.dev** | Vuln DB | Package vulnerability auditing (batch) |
| **UI Bridge** | Connected app | Live page content from connected apps |

**Free providers** (DuckDuckGo, Sploitus, OSV.dev) are always available — no configuration needed.

**Paid providers** (Tavily, Perplexity) require API keys via environment variables. They only activate when free providers return insufficient results.`,
      estimatedDuration: 1,
      tips: [
        "The Stats tab shows which providers have been used and their success rates",
        "DuckDuckGo has a 2-second rate limiter to avoid IP blocking",
        "Tavily uses 'advanced' depth for API documentation queries",
      ],
    },
    {
      id: "search-modes",
      title: "Three Search Modes",
      content: `The Knowledge Explorer offers three search modes, each optimized for a different use case:

**Web Search** — General-purpose web search. Queries DuckDuckGo, escalates to Tavily/Perplexity if results are sparse. Best for error messages, "how to" questions, and quick fact-checking.

**Error Research** — Compound search optimized for debugging. Accepts an error message, optional framework context, and stack trace. Builds a richer query by extracting the first meaningful stack frame. Results are tagged with \`ErrorResolution\` domain.

**Vulnerability Search** — Queries Sploitus for known exploits by software name, version, or CVE ID. Returns exploit source code (capped at 50KB), CVSS severity scores, and links. Results include severity badges: Critical (≥9.0), High (≥7.0), Medium (≥4.0), Low.`,
      estimatedDuration: 1,
      details: `### How the Fix Agent Uses Knowledge Acquisition

When the orchestration loop enters the fix phase, it:
1. Extracts error descriptions from reflection fixes
2. Truncates to 200 characters (safe UTF-8 boundary)
3. Calls \`search_with_context()\` with a 15-second timeout
4. Appends up to 3 results as "External Research Context" sections to the fix prompt
5. The fix agent then has real web sources to inform its code changes

This happens automatically — you don't need to trigger it. The fix agent gets smarter with every search it performs.`,
    },
    {
      id: "vulnerability-intelligence",
      title: "Vulnerability Intelligence",
      content: `The system provides three levels of vulnerability intelligence:

**1. Manual search** — Use the Vuln Search mode in the Explorer to search by CVE ID (e.g., \`CVE-2021-44228\`) or software name + version (e.g., \`apache 2.4.49\`).

**2. Dependency audit** — The \`/knowledge/audit-manifest\` endpoint parses \`package.json\`, \`Cargo.toml\`, or \`pyproject.toml\` and batch-queries OSV.dev for all dependencies. Supports npm, PyPI, crates.io, Go, Maven, and NuGet.

**3. Auto-enrichment** — When a Security-category finding is detected during workflow execution, an async task spawns that extracts CVE IDs from the description, queries Sploitus for exploit data, and stores the enrichment (CVSS scores, exploit count, remediation) as a \`vulnerability_enrichment\` knowledge entry.`,
      estimatedDuration: 1,
      tips: [
        "OSV.dev batch queries support up to 1000 packages at once",
        "Workspace Cargo.toml files are rejected with a descriptive error — use member crate Cargo.toml",
        "Semver prefixes (^, ~, >=) are stripped automatically during manifest parsing",
      ],
    },
    {
      id: "observability",
      title: "Observability & Stats",
      content: `The **Stats tab** shows a real-time dashboard of knowledge acquisition activity:

**Top-level metrics:**
- **Total Searches** — cumulative count since process start
- **Cache Hit Rate** — % of searches served from Tier 0 (green > 50%, yellow otherwise)
- **Results Stored** — entries ingested into \`task_knowledge\`
- **Dedup Skipped** — duplicate results detected and not re-stored

**Per-provider breakdown:**
Each provider shows queries, successes, failures, timeouts, and success rate. Providers with failures appear in red. Only providers that have handled at least one query are shown.

**Injection detections:**
Count of ingested results flagged by the prompt injection scanner (shown in yellow when > 0).`,
      estimatedDuration: 1,
      tips: [
        "Stats auto-refresh every 30 seconds",
        "The uptime counter shows how long since the runner started",
        "All counters use atomic operations — they're thread-safe and accurate under concurrency",
      ],
    },
    {
      id: "prompt-injection",
      title: "Prompt Injection Protection",
      content: `Since the system ingests content from **untrusted external sources** (web pages, exploit databases), every result passes through a **prompt injection scanner** before storage.

The scanner detects:
- **21 injection phrases** — "ignore previous instructions", "system prompt:", "jailbreak", etc.
- **6 exfiltration patterns** — "exfiltrate", "send api key to", "send credentials to", etc.
- **16 invisible Unicode characters** — zero-width spaces, RTL overrides, word joiners

**Important:** Flagged content is **NOT dropped**. It's stored with an \`[INJECTION_WARNING]\` prefix listing the matched patterns. This follows the principle of **transparency over silent removal** — the content may be legitimate documentation about prompt injection that happens to match patterns.`,
      estimatedDuration: 1,
      details: `### Why Not Block?

Web search results about security topics (e.g., "how to prevent prompt injection") will inevitably contain the very phrases the scanner looks for. Blocking them would create false positives that make the vulnerability search useless.

Instead, the warning prefix lets downstream consumers (the LLM, the fix agent) make informed decisions. The injection_detections counter in Stats helps you monitor the rate.`,
    },
    {
      id: "dual-database",
      title: "Dual Database Backend",
      content: `The knowledge acquisition system supports both **SQLite** and **PostgreSQL** backends, following the runner's migration strategy:

- **SQLite** (\`CheckpointDb\`) — always available, used as fallback
- **PostgreSQL** (\`PgDb\`) — used when \`DATABASE_URL\` env var is set

All operations (Tier 0 lookup, Tier 3 insert, embedding storage, dedup check) **prefer PG when available** and fall back to SQLite transparently. The \`SearchContext\` object carries both database handles.

Embeddings use the same format on both backends: **MiniLM-L6-v2** (384 dimensions), stored as little-endian f32 bytes in BLOB (SQLite) or BYTEA (PG). The same \`rank_results()\` algorithm handles in-memory cosine similarity ranking for both.`,
      estimatedDuration: 1,
      tips: [
        "The PG table is auto-created via ensure_tables() on startup — no manual migration needed",
        "Timestamps are ISO 8601 formatted via to_char() in PG for consistency with SQLite",
        "The system never dual-writes — data goes to PG or SQLite, not both",
      ],
    },
    {
      id: "mcp-tools",
      title: "MCP Tool Integration",
      content: `Six knowledge acquisition tools are registered in the **AgentToolRegistry** for autonomous use by Claude during workflow execution:

| Tool | Role | When Claude Uses It |
|------|------|-------------------|
| \`search_web\` | Researcher | Quick fact-checking during coding |
| \`research_error\` | Researcher | Debugging unfamiliar errors |
| \`lookup_api_docs\` | Researcher | Finding API documentation |
| \`search_vulnerabilities\` | Security | Checking for known exploits |
| \`audit_dependencies\` | Security | Scanning package versions |
| \`audit_manifest\` | Security | Parsing + auditing manifest files |

Each tool has a descriptive LLM-friendly description that helps the AI choose the right tool. For example, \`search_web\` says *"For deep research, use research_error or lookup_api_docs instead"*, guiding Claude away from the wrong tool.

The **Query Tool** also supports a \`web\` query type, accessible via the standard \`/query\` endpoint with \`query_type: "web"\`.`,
      estimatedDuration: 1,
    },
    {
      id: "api-endpoints",
      title: "HTTP API Reference",
      content: `Nine endpoints under \`/knowledge/*\`:

| Method | Endpoint | Purpose |
|--------|----------|---------|
| GET | \`/knowledge/providers\` | List provider availability |
| GET | \`/knowledge/stats\` | Observability metrics |
| POST | \`/knowledge/search-web\` | General web search |
| POST | \`/knowledge/research-error\` | Error research with escalation |
| POST | \`/knowledge/fetch-page\` | UI Bridge snapshot fetch |
| POST | \`/knowledge/lookup-api-docs\` | Deep API docs search |
| POST | \`/knowledge/search-vulnerabilities\` | Exploit/CVE search |
| POST | \`/knowledge/audit-dependencies\` | Batch package audit |
| POST | \`/knowledge/audit-manifest\` | Parse manifest + audit |

All POST endpoints accept JSON and return \`{ success, data: { results, provider_used } }\`. Provider failures never propagate as errors — the system degrades gracefully to fewer results.`,
      estimatedDuration: 1,
      resources: [
        {
          title: "OSV.dev API Documentation",
          url: "https://osv.dev/docs/",
          type: "api-reference",
        },
        {
          title: "Tavily API",
          url: "https://docs.tavily.com/",
          type: "api-reference",
        },
      ],
    },
    {
      id: "summary",
      title: "Putting It All Together",
      content: `The Knowledge Acquisition system turns the runner from an **isolated executor** into a **learning system** that can research unfamiliar problems in real-time.

**Key takeaways:**
- **Tiered search** minimizes cost: local cache → free providers → paid providers
- **The flywheel** means every search makes future searches faster
- **Auto-enrichment** adds vulnerability context to security findings without manual intervention
- **Stats** let you monitor the system's effectiveness (cache hit rate is the key metric)
- **Injection scanning** protects against malicious web content without blocking legitimate results
- **Dual-database** support ensures everything works during the SQLite→PG migration

To explore the system hands-on, switch to the **Search tab** and try a query — you'll see the tiered routing in action through the provider badges on each result.`,
      estimatedDuration: 1,
    },
  ],
};
