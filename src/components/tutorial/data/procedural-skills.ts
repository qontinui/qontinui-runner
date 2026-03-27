import type { Tutorial } from "@/types/tutorial";

/**
 * Informational tutorial: Procedural Skills System
 *
 * Explains the hermes-agent-inspired self-improving skill system —
 * how skills are extracted, approved, injected, and self-repaired.
 * Covers the full stack: cross-run learning, knowledge graph,
 * event bus, prompt injection, and the approval UI.
 */
export const proceduralSkillsTutorial: Tutorial = {
  id: "procedural-skills",
  title: "Procedural Skills: Self-Improving Agent Memory",
  description:
    "Understand how the runner learns reusable fix patterns from successful workflows and injects them into future agent prompts. Covers skill extraction, approval, knowledge graph integration, self-repair, and prompt injection detection.",
  duration: "12 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  category: "AI Features",
  tags: [
    "skills",
    "hermes-agent",
    "procedural-memory",
    "cross-run-learning",
    "knowledge-graph",
    "self-repair",
    "featured",
  ],
  prerequisites: ["getting-started"],
  learningObjectives: [
    "Understand what procedural skills are and why they exist",
    "Know how skills are auto-extracted from successful workflow runs",
    "Understand the approval lifecycle (pending, approved, rejected)",
    "Know how approved skills are injected into worker prompts",
    "Understand the self-repair loop that auto-disables failing skills",
    "Know how skills integrate with the knowledge graph and event bus",
    "Understand prompt injection detection on ingested content",
  ],
  steps: [
    {
      id: "intro",
      title: "What Are Procedural Skills?",
      content: `When a workflow struggles — iterating 3+ times before succeeding — the effective fix pattern is valuable. Rather than losing that knowledge, the runner captures it as a **procedural skill**: a reusable template that can guide future workflows facing similar problems.

This is inspired by [hermes-agent](https://github.com/NousResearch/hermes-agent)'s self-improving procedural memory. The key insight: **experience should compound into refined procedural knowledge without any model fine-tuning.**`,
      estimatedDuration: 1,
      tips: [
        "Skills are different from generation rules — rules guide workflow creation, skills guide the worker agent during execution",
        "The system is fully automatic: extraction, injection, and self-repair all happen without human intervention (except approval)",
      ],
    },
    {
      id: "extraction-pipeline",
      title: "How Skills Are Extracted",
      content: `After a reflection run completes, the **cross-run learning** system analyzes the results. If a task run meets these criteria, its effective fixes become skill candidates:

1. **Run succeeded** (status = 'completed')
2. **Required effort** (3+ iterations to succeed)
3. **Fix was effective** (effectiveness = 'effective')
4. **Fix was reused** (reuse_count >= 2, the quality gate)
5. **Not already captured** (dedup check by fix ID)

Each qualifying fix becomes a skill with \`source='auto'\` and \`approval_status='pending'\`.`,
      estimatedDuration: 1,
      details: `The extraction runs in \`cross_run_learning::extract_procedural_skills_detailed()\`. The SQL query filters on \`reflection_fixes\` where \`effectiveness = 'effective' AND reuse_count >= 2\`. Each fix generates a skill with:

- A slug derived from the fix type and affected file (normalized for Windows paths, truncated to 80 chars)
- Tags including \`["auto", "<fix_type>"]\`
- A single-step prompt template containing the fix description
- \`source_fix_id\` and \`source_pattern_id\` foreign keys for knowledge graph linking
- \`allowed_phases = ["agentic"]\` — skills only apply during the agentic phase`,
      tips: [
        "The quality gate (reuse_count >= 2) ensures only repeatedly-effective fixes become skills — not one-off lucky fixes",
        "Skills are also mirrored to PostgreSQL observations (topic_key='skill:<slug>') for cross-session full-text search",
      ],
    },
    {
      id: "approval-lifecycle",
      title: "The Approval Lifecycle",
      content: `Auto-extracted skills are **never injected directly**. They start as \`pending\` and require human review:

- **Pending** — Newly extracted, awaiting review. Not injected into prompts.
- **Approved** — Reviewed and accepted. Will be injected into future worker prompts.
- **Rejected** — Reviewed and declined. Hidden from injection, retained for audit.

The **Skills page** (sidebar > OBSERVE > Skills) shows all auto-extracted skills with filter tabs for each status. You can approve, reject, or reset skills with one click.`,
      estimatedDuration: 1,
      tips: [
        "The self-repair system can also revert approved skills back to pending — see the Self-Repair step",
        "Stale pending skills (30+ days, 0 usage) are auto-deleted during database maintenance",
      ],
    },
    {
      id: "prompt-injection",
      title: "How Skills Enter Worker Prompts",
      content: `When a workflow enters the **agentic phase**, the orchestrator calls \`build_skill_context()\` to query approved auto-skills. The result is injected into the worker's system prompt:

\`\`\`
## Procedural Skills (Auto-Learned)

The following fix patterns were learned from previous
successful runs. Apply them if they match the current issue:

- **Auto: Fix selector** (\`auto-selector-fix-login\`) [testing]: Use #email-input instead of .login-field
\`\`\`

**Progressive disclosure**: only skill names and first-sentence descriptions are shown. The worker sees enough context to recognize relevant patterns without prompt bloat.`,
      estimatedDuration: 1,
      details: `The injection is governed by two limits:

- **Count limit**: Maximum 10 skills per prompt (ordered by usage_count DESC)
- **Token budget**: Maximum ~2000 characters (~500 tokens) of skill context

If the budget is exceeded, remaining skills are noted as "N more available (token budget reached)." This prevents skills from consuming too much of the worker's context window.`,
    },
    {
      id: "self-repair",
      title: "Self-Repair: Auto-Disabling Bad Skills",
      content: `Not every approved skill helps. Some may cause regressions or be irrelevant to new workflows. The **self-repair loop** monitors this:

1. After each **failed verification** during a workflow run, the system checks which approved auto-skills were active
2. Each failure is recorded as a \`skill_failure\` event in \`task_run_events\`
3. After **3 consecutive failures** with the same skill active, the skill is automatically **reverted to pending**

This creates a negative feedback loop: skills that don't help get removed from circulation without human intervention.`,
      estimatedDuration: 1,
      tips: [
        "The failure tracking is per-skill, per-task-run — not global. A skill failing in one workflow doesn't affect its status in others",
        "Auto-disabled skills appear in the Skills panel's Pending tab after a Refresh, so you can re-review them",
      ],
    },
    {
      id: "knowledge-graph",
      title: "Skills in the Knowledge Graph",
      content: `Skills are first-class nodes in the runner's **knowledge graph** (powered by petgraph). Each auto-extracted skill becomes a \`Skill\` node with edges:

- **DerivedFrom** — links the skill to the \`Finding\` it was extracted from (via \`source_fix_id\` FK)
- **UsedIn** — links the skill to \`Workflow\` nodes where it was applied (via step provenance)
- **Supersedes** — links forked skills to their parent (when a skill is forked and improved)

This enables graph queries like:
- "What skills have been effective for this type of error?"
- "Which skills are derived from recurring patterns?"
- "What's the provenance chain for this fix?"`,
      estimatedDuration: 1,
      details: `The graph is materialized by \`graph_engine.rs\` methods:

- \`load_skills()\` — queries \`user_skills\` table, creates Skill nodes with properties (category, source, usage_count, version, approval_status)
- \`link_skills()\` — creates DerivedFrom edges via \`source_fix_id\` FK join through \`reflection_fixes\` to \`task_run_findings\`, and via \`source_pattern_id\` FK to \`cross_run_patterns\`
- Observation nodes with \`topic_key='skill:<slug>'\` also get \`InformedBy\` edges to their corresponding Skill node

The graph summary is available at \`GET /graph/summary\` — check \`nodes_by_kind.skill\` for the count.`,
    },
    {
      id: "event-bus",
      title: "Skill Events on the Workflow Event Bus",
      content: `The Inngest-inspired **workflow event bus** emits three skill-related events:

| Event | When | Data |
|-------|------|------|
| \`skill.created\` | Auto-extraction completes | skill_id, slug, source_fix_id, source_task_run_id |
| \`skill.updated\` | Skill is revised | skill_id, revision details |
| \`skill.failed\` | Skill-informed iteration fails | skill_id, slug, iteration, consecutive_failures, auto_disabled |

These events enable downstream subscribers (triggers, watchers, analytics) to react to skill lifecycle changes. For example, a trigger could notify the user when a skill is auto-disabled.`,
      estimatedDuration: 1,
    },
    {
      id: "prompt-injection-detection",
      title: "Prompt Injection Detection",
      content: `The **knowledge acquisition** system ingests content from external sources (DuckDuckGo, Tavily, Perplexity, UI Bridge). Before storage, all content passes through a **prompt injection scanner**:

- **23 injection patterns** detected (e.g., "ignore previous instructions", "system prompt:", "jailbreak")
- **7 exfiltration patterns** detected (e.g., "send credentials to", "exfiltrate", "upload private key")
- **16 invisible Unicode characters** detected (zero-width spaces, RTL overrides, BOM)

Suspicious content is **not blocked** — it's **flagged** with an inline \`[INJECTION_WARNING: ...]\` prefix for transparency. This follows the principle of transparency over silent removal.`,
      estimatedDuration: 1,
      tips: [
        "The patterns were tuned to avoid false positives on common code content — 'fetch()', 'process.env', 'curl' do NOT trigger the scanner",
        "There's a regression test confirming common code patterns don't false-positive",
      ],
    },
    {
      id: "context-compression",
      title: "Structured Context Compression",
      content: `Long-running workflows accumulate iteration context that can blow the token budget. The **context summarizer** compresses older iterations using a structured narrative template (also inspired by hermes-agent):

**Goal** — What the workflow is trying to achieve
**Progress** — What was accomplished so far
**Key findings** — Issues discovered
**Decisions** — Why certain approaches were chosen
**Files modified** — Which files were touched
**Next steps** — What remains to be done

This preserves actionable context much better than flat text compression. When asked to summarize, the AI fills in this template; on subsequent compressions, it **merges** into the existing summary rather than starting fresh.`,
      estimatedDuration: 1,
    },
    {
      id: "metrics-api",
      title: "Monitoring: Skill Metrics API",
      content: `The skill system exposes metrics at \`GET /graph/skill-metrics\`:

\`\`\`json
{
  "total_auto_skills": 5,
  "pending": 2,
  "approved": 2,
  "rejected": 1,
  "total_usage": 14,
  "failure_events": 3,
  "unique_skills_failed": 1,
  "top_skills": [
    {"slug": "auto-selector-fix-login", "category": "testing", "usage_count": 8}
  ]
}
\`\`\`

The **health endpoint** (\`GET /health\`) also includes \`consoleErrorCount\` for passive monitoring of JS errors without extra API calls.`,
      estimatedDuration: 1,
    },
    {
      id: "ui-bridge-improvements",
      title: "UI Bridge Testing Improvements",
      content: `Building the skills system surfaced UI Bridge testing gaps. Three new endpoints were added:

**\`POST /ui-bridge/control/assert\`** — Declarative UI assertions without JavaScript:
\`\`\`json
{"query": "h2", "queryType": "css", "assertion": "hasText", "expected": "Procedural Skills"}
\`\`\`

**\`POST /ui-bridge/control/page/evaluate-batch\`** — Multiple JS expressions in one round-trip (prevents lazy-component re-render races)

**\`POST /ui-bridge/control/page/evaluate-safe\`** — JS evaluation with try/catch wrapping (errors return as JSON, not connection drops)

The regular \`page/evaluate\` endpoint also gained a **direct WebView fallback** — if IPC to the SDK times out, it evaluates JS directly via Tauri's \`window.eval()\`.`,
      estimatedDuration: 1,
      tips: [
        "The assert endpoint supports 7 assertion types: exists, notExists, count, isVisible, hasText, hasClass, hasAttribute",
        "Batch evaluate is limited to 50 expressions per call",
      ],
    },
    {
      id: "builtin-skill",
      title: "The First Builtin Skill",
      content: `The system ships with one builtin skill: **UI Bridge Health Verify**. It's a 4-step verification template:

1. **Focus Window** — calls \`/gui-config/focus-window\` to bring the Tauri window to front
2. **Discover Elements** — forces UI Bridge element discovery
3. **Snapshot & Count** — verifies element count meets a minimum threshold
4. **Console Errors** — checks for JavaScript errors

This skill demonstrates how procedural skills work as composable workflow steps. It can be added to any workflow's verification phase via the skill library picker.`,
      estimatedDuration: 1,
    },
    {
      id: "summary",
      title: "The Complete Picture",
      content: `The procedural skills system creates a **closed learning loop**:

\`\`\`
Workflow fails → iterates → succeeds → reflection extracts fix
    ↓
Fix reused 2+ times → skill extracted (pending)
    ↓
Human approves → skill injected into future prompts
    ↓
Skill helps → usage_count++ | Skill hurts → auto-disabled after 3 failures
    ↓
Knowledge graph tracks provenance | Event bus notifies subscribers
\`\`\`

**Key architectural decisions:**
- Skills require human approval before injection (safety gate)
- Quality gate (reuse_count >= 2) filters noise
- Token budget (2000 chars) prevents prompt bloat
- Self-repair loop prevents bad skills from persisting
- Stale cleanup (30 days) prevents table bloat

The system is designed to compound knowledge over time without model fine-tuning — every successful complex workflow makes the next one slightly easier.`,
      estimatedDuration: 1,
      tips: [
        "Check GET /graph/skill-metrics to see how your skill system is performing",
        "The Skills page in the sidebar shows all pending skills awaiting your review",
      ],
    },
  ],
};
