import type { Tutorial } from "../../../types/tutorial";

/**
 * Cost Control Panel — Informational Tutorial
 *
 * Explains what the cost optimization engine is, what each section of the
 * Cost Control tab shows, and how the system protects against runaway costs.
 */
export const costControlTutorial: Tutorial = {
  id: "cost-control",
  title: "Understanding the Cost Control Panel",
  description:
    "Learn how Qontinui tracks, controls, and optimizes AI spending in real time. Covers budget tracking, circuit breakers, anomaly detection, and cost recommendations.",
  duration: "6 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  focusPage: "cost-control",
  category: "Observability",
  tags: ["cost", "budget", "observability", "featured"],
  learningObjectives: [
    "Understand how per-run budget tracking works",
    "Read the budget gauge and summary cards",
    "Know what the circuit breaker does and when it trips",
    "Interpret anomaly detection alerts",
    "Use cost recommendations to reduce spend",
  ],
  steps: [
    {
      id: "cc-overview",
      title: "What is the Cost Control Panel?",
      content:
        "The **Cost Control Panel** is the real-time command center for AI spending. Every AI call during a workflow run is tracked, costed, and fed through three safety systems:\n\n" +
        "- **Budget Tracker** — enforces per-run spending limits\n" +
        "- **Circuit Breaker** — halts execution on cost anomalies\n" +
        "- **Anomaly Detector** — flags statistically unusual costs\n\n" +
        "Data flows in real time via event subscriptions — no polling. When a run is idle, the panel shows historical cost data from the last 30 days.",
      estimatedDuration: 1,
      tips: [
        "The panel updates live during active runs — you don't need to refresh manually.",
        "Historical data (summary cards, phase chart) comes from PostgreSQL aggregations.",
      ],
      details:
        "**Architecture**: Cost events emit through the EventBroadcaster to both Tauri IPC (desktop UI) and WebSocket (GraphQL subscriptions). " +
        "The backend creates a `RunCostTrackers` bundle per execution — containing a BudgetTracker, CostCircuitBreaker, and CostAnomalyDetector — " +
        "registered in AppState by execution_id and cleaned up on run completion.",
    },
    {
      id: "cc-summary-cards",
      title: "Summary Cards — The Big Picture",
      content:
        "These four cards give you a 30-day snapshot of your AI spending:\n\n" +
        "- **Total Cost** — aggregate USD across all runs\n" +
        "- **Total Tokens** — combined input + output tokens\n" +
        "- **Cache Hit Rate** — what percentage of tokens hit the prompt cache (higher is better)\n" +
        "- **Cache Savings** — estimated dollars saved by caching (green = good)\n\n" +
        "These refresh automatically when cost events fire during a run, with a 300ms debounce to avoid flickering.",
      targetElement: {
        selector: '[data-tutorial-id="cc-summary-cards"]',
        highlightType: "border",
        position: "bottom",
        scrollIntoView: true,
      },
      estimatedDuration: 1,
      tips: [
        "A cache hit rate below 20% means your prompts aren't leveraging Anthropic's prompt caching effectively.",
        "Cache savings are estimated at $2.70/MTok based on Sonnet pricing — the actual savings depend on the model used.",
      ],
    },
    {
      id: "cc-budget-gauge",
      title: "Budget Gauge — Live Spend Tracking",
      content:
        "During an active run, the **circular gauge** shows real-time budget consumption:\n\n" +
        "- **Green** — under 60% used, healthy\n" +
        "- **Yellow** — 60-80% used, approaching limit\n" +
        "- **Red** — over 80%, danger zone\n\n" +
        'Below the gauge: exact dollar amounts ("$1.23 / $5.00") and remaining percentage.\n\n' +
        "When no run is active, you'll see a \"No active run\" placeholder. The gauge appears automatically when a workflow starts.",
      targetElement: {
        selector: '[data-tutorial-id="cc-budget-gauge"]',
        highlightType: "spotlight",
        position: "right",
        scrollIntoView: true,
      },
      estimatedDuration: 1,
      details:
        "**How budgets work**: Each run gets a default budget of **$5.00 / 500K tokens**, split across phases: " +
        "agentic (65%), verification (15%), generation (10%), setup (5%), completion (5%). " +
        "When the budget is exceeded, the `BudgetEnforcementMiddleware` injects conciseness instructions into the prompt, " +
        "and if spending continues, the run returns a `BudgetExceeded` outcome and stops gracefully.",
    },
    {
      id: "cc-circuit-breaker",
      title: "Circuit Breaker — The Safety Net",
      content:
        "The **Circuit Breaker** is a hard safety mechanism that halts execution when something goes wrong:\n\n" +
        "- **Single call > $2.00** — one API call cost too much\n" +
        "- **Budget overspend > 150%** — cumulative cost far exceeds the budget\n" +
        "- **5 consecutive cache misses** — cache health degraded\n\n" +
        'When tripped, the badge turns **red** and shows "Tripped". The run stops immediately with a `BudgetExceeded` outcome.\n\n' +
        'When healthy, the badge is **green** showing "Closed".',
      targetElement: {
        selector: '[data-tutorial-id="cc-safety-section"]',
        highlightType: "spotlight",
        position: "top",
        scrollIntoView: true,
      },
      estimatedDuration: 1,
      tips: [
        "The circuit breaker resets automatically for each new run.",
        "Below the badge, you can see the anomaly detector's sample count and rolling mean cost.",
      ],
    },
    {
      id: "cc-anomaly-detection",
      title: "Anomaly Detection — Statistical Cost Monitoring",
      content:
        "The **Anomaly Detector** uses Welford's online algorithm to maintain a rolling mean and standard deviation of per-call costs.\n\n" +
        "After accumulating 10 samples, any call costing more than **mean + 2 standard deviations** (z-score > 2.0) is flagged.\n\n" +
        "Each anomaly entry shows:\n" +
        "- **z-score** — how many standard deviations above normal\n" +
        "- **Cost** — what the anomalous call cost\n" +
        "- **Mean** — what the average call costs\n\n" +
        "When no anomalies exist, you'll see \"No anomalies detected\" — which is the normal, healthy state.",
      targetElement: {
        selector: '[data-tutorial-id="cc-safety-section"]',
        highlightType: "border",
        position: "top",
        scrollIntoView: true,
      },
      estimatedDuration: 1,
      details:
        "**Why Welford's algorithm?** It computes mean and variance incrementally without storing all samples, using only O(1) memory. " +
        "This is important because the detector runs after every AI call in the hot path. " +
        "The `check()` method is read-only (doesn't mutate state), and `update()` is called separately to incorporate the sample.",
    },
    {
      id: "cc-cost-feed",
      title: "Cost Event Feed — Per-Call Transparency",
      content:
        "The **Recent Cost Events** table shows every AI call in reverse chronological order (newest first, max 50 displayed, 200 buffered).\n\n" +
        "Columns:\n" +
        "- **Time** — when the call completed\n" +
        "- **Phase** — which workflow phase (agentic, verification, etc.)\n" +
        "- **Iter** — iteration number within the phase\n" +
        "- **Tokens** — input/output token counts\n" +
        "- **Cost** — per-call cost in USD\n" +
        "- **Cumulative** — running total for the run\n" +
        "- **Cache** — cache hit rate for this call\n\n" +
        "Cost and Cumulative columns use monospace font for easy comparison.",
      estimatedDuration: 1,
      tips: [
        "The cumulative column should always increase — if it doesn't, there may be a recording issue.",
        "Low cache percentages on individual calls are normal for the first call; subsequent calls should improve.",
      ],
    },
    {
      id: "cc-recommendations",
      title: "Optimization Recommendations",
      content:
        "The **Optimization Recommendations** section has two sources:\n\n" +
        "**Meta-optimizer recommendations** — generated by analyzing your historical data:\n" +
        "- **Cache efficiency** — identifies phases with low cache hit rates\n" +
        "- **Duplicate prompts** — detects near-identical prompts that waste tokens\n" +
        "- **Model downgrade** — finds phases where a cheaper model would perform equally well\n\n" +
        "**Dashboard insights** — derived from the current dashboard data:\n" +
        "- Low cache hit rate warnings (< 30%)\n" +
        "- High budget utilization alerts (> 85%)\n" +
        "- Dominant phase spending (one phase > 60% of total)\n\n" +
        "Insights are color-coded: blue (info), yellow (warning), red (critical).",
      estimatedDuration: 1,
      tips: [
        "Recommendations accumulate over time — run more workflows to get more accurate suggestions.",
        "Each recommendation shows a confidence percentage based on the amount of data behind it.",
      ],
    },
    {
      id: "cc-data-flow",
      title: "How It All Fits Together",
      content:
        "Here's the complete data flow:\n\n" +
        "1. **AI call completes** in the agentic executor\n" +
        "2. **Cost is calculated** using model-specific pricing\n" +
        "3. **BudgetTracker.record()** updates the running total\n" +
        "4. **CircuitBreaker** checks single-call and cumulative thresholds\n" +
        "5. **AnomalyDetector** checks against the rolling mean\n" +
        "6. **EventBroadcaster** emits cost-update to both Tauri and WebSocket\n" +
        "7. **Frontend** receives events, updates the feed table, and invalidates dashboard queries\n\n" +
        "If budget is exceeded or circuit breaker trips, the run stops with a `BudgetExceeded` outcome — visible in the run history as a graceful stop, not an error.",
      estimatedDuration: 1,
      details:
        "**LLM Analytics integration**: The LLM Analytics tab also listens for cost-update events and refreshes its aggregated views (cost by model, cost by phase, etc.) " +
        "with a 300ms debounce. This replaced the previous 60-second polling interval, making the analytics dashboard event-driven.",
    },
  ],
};
