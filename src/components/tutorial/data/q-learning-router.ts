/**
 * Q-Learning Architecture Router Tutorial (Informational)
 *
 * Explains the Q-learning system that learns which WorkflowArchitecture
 * (Traditional, AgenticVerification, MultiAgentPipeline) works best for
 * different types of tasks. Covers the state space, Q-update math,
 * epsilon-greedy exploration, PG-first storage, and the frontend dashboard.
 */

import type { Tutorial } from "../../../types/tutorial";

export const qLearningRouterTutorial: Tutorial = {
  id: "q-learning-router",
  title: "Q-Learning Architecture Router",
  description:
    "Understand how the autoresearch system uses reinforcement learning to automatically route tasks to the best workflow architecture based on task characteristics.",
  duration: "12 minutes",
  difficulty: "advanced",
  mode: "contextual",
  focusPage: "help",
  category: "Architecture",
  tags: [
    "q-learning",
    "reinforcement-learning",
    "autoresearch",
    "architecture-routing",
    "meta-optimizer",
    "featured",
  ],
  prerequisites: ["getting-started"],
  learningObjectives: [
    "Understand why architecture routing exists and what problem it solves",
    "Know the 50-state task discretization scheme",
    "Understand the Q-update formula and reward computation",
    "Know how epsilon-greedy exploration balances learning vs. exploitation",
    "Understand the PG-first storage strategy and dual-write pattern",
    "Use the Q-Routing dashboard to monitor convergence and set overrides",
  ],
  steps: [
    {
      id: "intro",
      title: "The Architecture Routing Problem",
      content: `The runner supports **three workflow architectures**, each with different strengths:

| Architecture | Approach | Best for |
|-------------|----------|----------|
| **Traditional** | Deterministic verification + agentic fix loops | Well-defined tasks with known checks |
| **Agentic Verification** | Verification agent + worker agent loop | Adaptive tasks where checks aren't predefined |
| **Multi-Agent Pipeline** | DAG of specialized agents (spec analyst → locator → implementer → verifier) | Complex multi-file tasks |

**The problem:** Which architecture should the system use for a given task? Previously, autoresearch used grid search or random selection — treating architecture as a global optimization ("which is best overall?") without considering task characteristics.

**The solution:** A Q-learning router that learns *per-task-type* preferences. Frontend tasks might work best with Traditional, while complex backend refactors might benefit from Multi-Agent Pipeline.`,
      estimatedDuration: 1,
      tips: [
        'The Q-router is opt-in via mutation_strategy: "q_learning" in campaign config',
        "It does NOT replace existing grid/LLM/random strategies — it's an additional option",
      ],
    },
    {
      id: "state-space",
      title: "The 50-State Task Space",
      content: `To learn per-task preferences, the system discretizes tasks into a **50-state space** using three features extracted from every workflow outcome:

**Primary Domain** (5 values):
\`frontend\`, \`backend\`, \`database\`, \`testing\`, \`infra\`
→ Inferred from \`domain_tags\` on each learning outcome

**Complexity Tier** (5 values):
\`trivial\`, \`simple\`, \`moderate\`, \`complex\`, \`highly_complex\`
→ Computed from step counts, iterations, duration, and agentic step counts

**Has UI Component** (2 values):
\`true\` if technology tags contain react, css, html, or nextjs
→ Inferred from \`technology_tags\`

**Total:** 5 × 5 × 2 = **50 possible states**

Each state is serialized as a key like \`"frontend:moderate:ui"\` or \`"backend:complex:no_ui"\`.`,
      estimatedDuration: 1,
      details: `**Complexity tier scoring** (0–11 range):

- Step count: 0 (≤3), 1 (4–8), 2 (9–15), 3 (16–30), 4 (>30)
- Agentic steps: 0 (≤2), 1 (3–5), 2 (6–10), 3 (>10)
- Iterations: 0 (≤2), 1 (3–5), 2 (>5)
- Duration: 0 (≤120s), 1 (121–600s), 2 (>600s)

Tiers: trivial (0), simple (1), moderate (2–4), complex (5–7), highly_complex (8+)

Domain inference priority: frontend > database > testing > infra > backend (default)`,
    },
    {
      id: "q-table",
      title: "The Q-Table: Learning Architecture Preferences",
      content: `The Q-table maps **(state, action) → value**, where:
- **State** = one of the 50 task states
- **Action** = one of the 3 architectures
- **Value** = learned expected reward (0.0 to 1.0)

The table has at most **150 entries** (50 states × 3 architectures). Each entry stores:

\`\`\`
q_value:     0.723   (learned expected reward)
visit_count: 15      (how many times this pair was observed)
\`\`\`

**Higher Q-value = better expected performance** for that architecture on that task type.

For example, if \`Q(frontend:moderate:ui, agentic_verification) = 0.85\` and \`Q(frontend:moderate:ui, traditional) = 0.42\`, the router learns that Agentic Verification is the better choice for moderate frontend tasks with UI components.`,
      estimatedDuration: 1,
      tips: [
        "The Q-table is stored in both PostgreSQL (primary) and SQLite (fallback)",
        "View the live Q-table in the Autoresearch page → Q-Routing tab",
      ],
    },
    {
      id: "q-update",
      title: "The Q-Update Formula",
      content: `After every workflow completion, the Q-value for the observed (state, architecture) pair is updated:

\`\`\`
Q(s, a) ← Q(s, a) + α × (reward − Q(s, a))
\`\`\`

Where:
- **α = 0.1** — Learning rate (how fast new observations overwrite old beliefs)
- **reward** — Computed from the workflow outcome

**Reward computation:**
\`\`\`
reward = composite_agentic_score − 0.01 × total_cost_usd
\`\`\`
Clamped to [0.0, 1.0].

The \`composite_agentic_score\` is a weighted blend of task completion (25%), goal accuracy (20%), step efficiency (12%), tool correctness (8%), and other agentic metrics. The cost penalty slightly favors cheaper architectures when performance is equal.

**Key insight:** Failures (score = 0.0) intentionally update the Q-table with reward ≈ 0, penalizing architectures that fail. Only invalid scores (< 0) are skipped.`,
      estimatedDuration: 2,
      details: `**Why α = 0.1?**

A learning rate of 0.1 means each new observation shifts the Q-value by 10% toward the new reward. This provides:
- **Stability** — A single outlier doesn't drastically change routing
- **Adaptability** — After ~20 observations, the Q-value closely tracks the true average
- **Recency bias** — Recent outcomes matter slightly more than old ones

After 100 observations of reward = 0.8, the Q-value converges to within 0.01 of 0.8. This is verified by the \`test_q_update_converges\` unit test.`,
    },
    {
      id: "epsilon-greedy",
      title: "Epsilon-Greedy Exploration",
      content: `The router uses **epsilon-greedy** policy to balance exploration (trying new architectures) vs. exploitation (using the best known architecture):

- With probability **ε**: pick a random architecture (explore)
- With probability **1 − ε**: pick the architecture with the highest Q-value (exploit)

**Epsilon auto-decays per state** as visits accumulate:
\`\`\`
ε = max(0.05, 0.3 × exp(−total_visits / 20))
\`\`\`

| Total visits | ε (exploration rate) | Behavior |
|-------------|---------------------|----------|
| 0 | 0.30 (30%) | Heavy exploration |
| 10 | 0.18 (18%) | Moderate exploration |
| 20 | 0.11 (11%) | Light exploration |
| 40 | 0.05 (5%) | Near-pure exploitation |

**Cold start:** A state must have at least 1 visit per architecture (3 total) before Q-routing activates. Until then, the system falls back to sequential grid search.`,
      estimatedDuration: 1,
      tips: [
        "The confidence percentage in the Policy view is 1 − ε",
        "Manual overrides bypass epsilon-greedy entirely — the locked architecture is always used",
        "The minimum ε of 5% ensures the router never stops exploring completely",
      ],
    },
    {
      id: "manual-overrides",
      title: "Manual Overrides",
      content: `Sometimes you know better than the Q-table. **Manual overrides** let you lock a specific state to a specific architecture, bypassing Q-routing entirely.

**How overrides work:**
1. In the Q-Routing tab's **Policy view**, each state has an "Override" column
2. Select an architecture from the "Lock to..." dropdown
3. The override is written to both PostgreSQL and SQLite
4. All future routing decisions for that state return the locked architecture
5. Q-values continue to update normally (learning isn't paused)

**Override precedence:** \`select_architecture()\` checks overrides first, before epsilon-greedy. This means overrides always win.

**Removing overrides:** Click the "×" button next to the orange override badge. The state resumes normal Q-routing.

**Use cases:**
- Force Traditional for production-critical tasks while Q-table is still learning
- Lock a known-good architecture for a specific domain while experimenting elsewhere
- Override a bad Q-value that was poisoned by a flaky test run`,
      estimatedDuration: 1,
    },
    {
      id: "storage-strategy",
      title: "PG-First Storage Strategy",
      content: `The Q-routing system uses a **PG-first** dual-write strategy:

**Write path** (after each workflow completion):
1. \`record_workflow_learning()\` writes the learning outcome + agentic metrics to **SQLite**
2. \`update_q_routing_table()\` computes Q-update and writes to **SQLite**
3. \`update_q_routing_table_pg()\` independently computes Q-update against **PostgreSQL** state

PG and SQLite Q-values may diverge slightly in multi-instance deployments because each computes the Q-update against its own current state.

**Read path** (Tauri commands for the frontend):
1. Try PostgreSQL first → if connected and query succeeds, use PG results
2. Fall back to SQLite only on PG error or when PG is not configured

**Engine startup** (when a Q-learning campaign starts):
1. Try to load Q-table from PG
2. Fall back to SQLite if PG unavailable
3. Overrides always loaded from SQLite (engine doesn't have PG access during the campaign loop)

**Override writes** go to PG first, then SQLite, ensuring both stores stay in sync.`,
      estimatedDuration: 1,
      details: `**Why dual-write instead of PG-only?**

SQLite provides offline resilience — the runner can operate without a PostgreSQL connection. The Q-table is small (≤150 rows), so dual-write overhead is negligible. PG is preferred for reads because it's shared across multiple runner instances in a team deployment.

**Why independent Q-updates instead of mirroring?**

In multi-instance deployments, two runners might complete different workflows simultaneously. If both mirror SQLite → PG, they'd overwrite each other. Independent PG Q-updates let each runner contribute its own observations to the shared PG Q-table, which naturally converges via the α=0.1 learning rate.`,
    },
    {
      id: "autoresearch-integration",
      title: "Integration with Autoresearch Campaigns",
      content: `Q-routing is activated by setting \`mutation_strategy: "q_learning"\` in an autoresearch campaign configuration.

**How the QLearningMutator works:**
1. On campaign start, loads the Q-table and overrides from the database
2. For each experiment, the sequential fallback handles non-architecture dimensions (model, max_iterations, etc.)
3. If \`WorkflowArchitecture\` is a search dimension AND the current task state has sufficient data, the Q-router overrides the architecture selection
4. Otherwise, falls back to grid search (sequential cycling through Traditional → AgenticVerification → MultiAgentPipeline)

**Seeding the Q-table:**
Run initial campaigns with \`mutation_strategy: "sequential"\` on the \`WorkflowArchitecture\` dimension. This ensures each architecture gets tried for each task type, building the baseline Q-values needed for Q-routing to activate.

**Other strategies are NOT removed.** Q-learning is an addition:
| Strategy | How it picks architecture |
|----------|-------------------------|
| \`sequential\` | Cycles through all three |
| \`random_perturbation\` | Picks randomly |
| \`ai_guided\` | LLM recommends based on history |
| \`q_learning\` | **Learned per-task preference** |`,
      estimatedDuration: 1,
    },
    {
      id: "dashboard",
      title: "The Q-Routing Dashboard",
      content: `The Q-Routing tab on the Autoresearch page has three views:

**Q-Values (Heatmap)**
- States × architectures grid
- Cells colored green by Q-value intensity
- Shows Q-value (3 decimals) and visit count per cell
- Best architecture highlighted with blue ring
- Override states show "(locked)" in orange

**Policy**
- One row per state with the recommended architecture
- Color-coded confidence (green ≥90%, blue ≥70%, yellow ≥50%)
- Active/Cold status badges
- Override dropdown ("Lock to...") per state

**Visit Counts**
- Same grid but colored by visit density (blue gradient)
- Identifies under-explored states
- Legend shows density scale (0 → 30+)

**Stats bar** at the top shows: States with Data, Routable States, State-Action Pairs, Total Visits, Avg Q-Value, and Coverage percentage.

**Reset Q-Table** button clears all Q-values (preserves overrides). Requires confirmation.

Data auto-refreshes every 10 seconds.`,
      estimatedDuration: 1,
    },
    {
      id: "data-flow",
      title: "End-to-End Data Flow",
      content: `Here's the complete lifecycle of a Q-routing update:

\`\`\`
Workflow completes
       ↓
learning_recorder records outcome to SQLite
       ↓
score_and_persist_agentic_metrics() computes composite score
       ↓
update_q_routing_table() extracts TaskState from tags,
  computes reward, updates SQLite Q-entry
       ↓
update_q_routing_table_pg() independently updates PG Q-entry
       ↓
QRoutingTab polls get_q_routing_table (PG-first)
       ↓
Dashboard reflects updated Q-values within 10 seconds
\`\`\`

**When a Q-learning campaign runs:**
\`\`\`
QLearningMutator.next_experiment() called
       ↓
Check override for current task state → if locked, return it
       ↓
Check has_sufficient_data() → if not, fall back to grid search
       ↓
Compute ε = max(0.05, 0.3 × exp(-visits/20))
       ↓
With probability ε → random architecture
With probability 1-ε → argmax Q(state, *)
\`\`\``,
      estimatedDuration: 1,
    },
    {
      id: "mental-model",
      title: "Mental Model Summary",
      content: `**The Q-learning router is a contextual bandit** — it learns which action (architecture) maximizes reward for each context (task state), without modeling sequential dependencies between actions.

**Key design decisions:**
- **Tabular, not deep:** 50 states × 3 actions = 150 entries. No neural networks needed.
- **Per-state epsilon decay:** States explored heavily converge to exploitation; new states explore freely.
- **Cost-penalized reward:** Architectures that cost more are slightly penalized, all else being equal.
- **Override-first policy:** Human expertise can always override learned preferences.
- **PG-first reads, dual-write:** Multi-instance teams share PG; single-instance runners have SQLite fallback.
- **Non-default:** Must opt in via \`mutation_strategy: "q_learning"\`. Existing strategies untouched.

**Files to know:**
| File | Purpose |
|------|---------|
| \`autoresearch/q_router.rs\` | Core Q-learning: state model, Q-table, policy, tests |
| \`autoresearch/mutations.rs\` | QLearningMutator wrapping QRouter |
| \`autoresearch/engine.rs\` | Campaign loop integration, PG/SQLite loading |
| \`autoresearch/commands.rs\` | 7 Tauri commands for dashboard + overrides |
| \`orchestrator/learning_recorder.rs\` | Q-update after each workflow outcome |
| \`database/pg/q_routing.rs\` | PostgreSQL CRUD for Q-table + overrides |
| \`QRoutingTab.tsx\` | Frontend dashboard component |`,
      estimatedDuration: 1,
      tips: [
        "The Q-table converges after ~20 visits per state — run diverse workloads to seed it",
        "Watch the Policy view's confidence column to track convergence",
        "The Reset Q-Table button preserves overrides — safe for re-learning after system changes",
      ],
    },
  ],
};
