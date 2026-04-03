/**
 * Service for agentic metric scores — deepeval-inspired workflow quality evaluation.
 *
 * Wraps the 4 Tauri commands: get_agentic_scores, get_agentic_metric_aggregates,
 * get_composite_score_trend, recompute_agentic_baselines.
 */

import { invoke } from "@tauri-apps/api/core";

export interface AgenticMetricScore {
  id: string;
  task_run_id: string;
  metric_type: string;
  score: number;
  confidence: number;
  rationale: string | null;
  is_llm_judged: boolean;
  model_used: string | null;
  created_at: string;
}

export interface AgenticMetricAggregate {
  metric_type: string;
  mean_score: number;
  min_score: number;
  max_score: number;
  runs_scored: number;
}

export interface CompositeScoreTrendPoint {
  date: string;
  avg_composite_score: number;
  run_count: number;
  success_count: number;
}

/** Metric display metadata. */
export const METRIC_INFO: Record<
  string,
  { label: string; category: "agentic" | "rag"; description: string }
> = {
  task_completion: {
    label: "Task Completion",
    category: "agentic",
    description: "How completely the task was accomplished",
  },
  step_efficiency: {
    label: "Step Efficiency",
    category: "agentic",
    description: "Execution efficiency vs baseline",
  },
  tool_correctness: {
    label: "Tool Correctness",
    category: "agentic",
    description: "Whether the right tools were used",
  },
  argument_correctness: {
    label: "Argument Correctness",
    category: "agentic",
    description: "Pipeline trace schema validity",
  },
  plan_adherence: {
    label: "Plan Adherence",
    category: "agentic",
    description: "How closely the plan was followed",
  },
  goal_accuracy: {
    label: "Goal Accuracy",
    category: "agentic",
    description: "Whether the stated goal was achieved",
  },
  plan_quality: {
    label: "Plan Quality",
    category: "agentic",
    description: "Quality of the execution plan",
  },
  rag_contextual_precision: {
    label: "RAG Precision",
    category: "rag",
    description: "Are top-ranked retrieval results relevant?",
  },
  rag_contextual_recall: {
    label: "RAG Recall",
    category: "rag",
    description: "Is important context being retrieved?",
  },
  rag_faithfulness: {
    label: "RAG Faithfulness",
    category: "rag",
    description: "Does output stay faithful to retrieved context?",
  },
  rag_answer_relevancy: {
    label: "RAG Relevancy",
    category: "rag",
    description: "Is retrieved context useful for the task?",
  },
  schema_compliance: {
    label: "Schema Compliance",
    category: "agentic",
    description:
      "Output schema validation success rate (first-attempt, after coercion, after retry)",
  },
};

export interface PushFeedbackScoresResult {
  pushed: number;
  failed: number;
  errors: string[];
}

export const agenticMetricsService = {
  /** Get all metric scores for a specific task run. */
  async getScoresForRun(taskRunId: string): Promise<AgenticMetricScore[]> {
    return invoke("get_agentic_scores", { taskRunId });
  },

  /** Get aggregate stats over a time period (default 30 days). */
  async getAggregates(days?: number): Promise<AgenticMetricAggregate[]> {
    return invoke("get_agentic_metric_aggregates", { days: days ?? 30 });
  },

  /** Get composite score trend over time. */
  async getCompositeScoreTrend(days?: number): Promise<CompositeScoreTrendPoint[]> {
    return invoke("get_composite_score_trend", { days: days ?? 30 });
  },

  /** Manually trigger baseline recomputation. */
  async recomputeBaselines(): Promise<number> {
    return invoke("recompute_agentic_baselines");
  },

  /** Push feedback scores for a specific task run to the backend. */
  async pushScoresToBackend(
    taskRunId: string,
    targetId: string,
    targetType: string,
  ): Promise<PushFeedbackScoresResult> {
    return invoke("push_agentic_scores_to_backend", { taskRunId, targetId, targetType });
  },

  /** Push the latest agentic scores to the backend. */
  async pushLatestScoresToBackend(
    targetId: string,
    targetType: string,
  ): Promise<PushFeedbackScoresResult> {
    return invoke("push_latest_agentic_scores", { targetId, targetType });
  },
};
