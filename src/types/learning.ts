/**
 * Learning Insights Dashboard types
 *
 * Types for the AI learning system that tracks task outcomes,
 * identifies patterns, and generates actionable insights.
 */

/** Outcome status of a completed task */
export type OutcomeStatus = "Success" | "PartialSuccess" | "Failure" | "Abandoned";

/** Outcome of a decision */
export type DecisionOutcome = "Positive" | "Negative" | "Neutral" | "Unknown";

/** Type of pattern */
export type PatternType =
  | "SuccessStrategy"
  | "FailureMode"
  | "ToolUsage"
  | "Collaboration"
  | "IterationBehavior"
  | "ErrorRecovery"
  | "DecisionMaking";

/** Category of insight */
export type InsightCategory =
  | "Performance"
  | "Quality"
  | "Efficiency"
  | "Reliability"
  | "Strategy"
  | "ToolUsage"
  | "Collaboration";

/** Performance trend direction */
export type Trend = "Improving" | "Declining" | "Stable" | "Insufficient";

/** Type of feedback */
export type FeedbackType =
  | "RecommendStrategy"
  | "AvoidStrategy"
  | "RecommendTool"
  | "AvoidTool"
  | "Warning"
  | "Optimization";

/** A decision made during task execution */
export interface Decision {
  /** What was decided */
  description: string;
  /** Why this decision was made */
  rationale: string | null;
  /** Alternatives considered */
  alternatives: string[];
  /** Outcome of the decision */
  outcome: DecisionOutcome;
  /** Context at the time of decision */
  context: Record<string, string>;
}

/** Represents the outcome of a completed task */
export interface TaskOutcome {
  /** Unique identifier for the task */
  task_id: string;
  /** Outcome status */
  status: OutcomeStatus;
  /** Duration in seconds */
  duration_secs: number | null;
  /** Number of iterations taken */
  iterations: number | null;
  /** Strategy used for the task */
  strategy: string | null;
  /** Agent roles involved */
  agents_involved: string[];
  /** Tools used during execution */
  tools_used: string[];
  /** Error messages if failed */
  errors: string[];
  /** Key decisions made during execution */
  decisions: Decision[];
  /** Metrics collected during execution */
  metrics: Record<string, number>;
  /** Timestamp when task completed */
  completed_at: string;
  /** Tags for categorization */
  tags: string[];
  /** Workflow architecture type */
  workflow_architecture?: string;
}

/** A pattern identified from task analysis */
export interface Pattern {
  /** Unique identifier */
  id: string;
  /** Pattern name */
  name: string;
  /** Description of the pattern */
  description: string;
  /** Pattern type */
  pattern_type: PatternType;
  /** Confidence score (0.0 - 1.0) */
  confidence: number;
  /** Number of occurrences observed */
  occurrences: number;
  /** Success rate when this pattern is observed */
  success_rate: number;
  /** Conditions that trigger this pattern */
  triggers: string[];
  /** Recommended actions when pattern is detected */
  recommendations: string[];
  /** Tasks where this pattern was observed */
  observed_in: string[];
  /** When pattern was first identified */
  identified_at: string;
  /** When pattern was last updated */
  updated_at: string;
}

/** Insight extracted from task analysis */
export interface Insight {
  /** Unique identifier */
  id: string;
  /** Insight category */
  category: InsightCategory;
  /** The insight content */
  content: string;
  /** Confidence score */
  confidence: number;
  /** Supporting evidence */
  evidence: string[];
  /** Actionable recommendations */
  recommendations: string[];
  /** Related patterns */
  related_patterns: string[];
  /** When insight was generated */
  generated_at: string;
}

/** Results from pattern analysis */
export interface AnalysisResult {
  /** Identified patterns */
  patterns: Pattern[];
  /** Generated insights */
  insights: Insight[];
  /** Overall success rate */
  success_rate: number;
  /** Average duration */
  avg_duration_secs: number | null;
  /** Average iterations */
  avg_iterations: number | null;
  /** Top performing strategies */
  top_strategies: Array<[string, number]>;
  /** Most used tools */
  top_tools: Array<[string, number]>;
  /** Overall performance trend */
  trend: Trend;
}

/** Feedback to apply to future tasks */
export interface Feedback {
  /** Feedback type */
  feedback_type: FeedbackType;
  /** Priority (higher = more important) */
  priority: number;
  /** The feedback content */
  content: string;
  /** Conditions when to apply */
  apply_when: string[];
  /** Source pattern or insight */
  source: string;
  /** Confidence score */
  confidence: number;
}

/** Learning system summary statistics */
export interface LearningSummary {
  total_tasks: number;
  successful_tasks: number;
  failed_tasks: number;
  patterns_identified: number;
  insights_generated: number;
  feedback_items: number;
  strategies_tracked: number;
  tools_tracked: number;
}

/** Combined dashboard data */
export interface LearningDashboardData {
  summary: LearningSummary;
  analysis: AnalysisResult;
  patterns: Pattern[];
  insights: Insight[];
}

// ============================================================================
// Enhanced Query Types (Filtering, Pagination, Date Ranges)
// ============================================================================

/** Filter options for learning outcomes query */
export interface LearningOutcomeFilter {
  status?: string;
  strategy?: string;
  since?: string;
  limit?: number;
}

/** Generic paginated result wrapper */
export interface PaginatedResult<T> {
  items: T;
  total: number;
  offset: number;
  limit: number;
}

/** Learning statistics for a date range */
export interface LearningStatsByDateRange {
  total: number;
  by_status: Record<string, number>;
  by_strategy: Record<string, number>;
  avg_duration_secs: number | null;
  avg_iterations: number | null;
  date_range: {
    start: string;
    end: string;
  };
}

/** Raw learning outcome from database */
export interface LearningOutcomeRecord {
  id: string;
  task_id: string;
  status: string;
  duration_secs: number | null;
  iterations: number | null;
  strategy: string | null;
  tools_used: string[] | null;
  files_modified: string[] | null;
  error_type: string | null;
  error_message: string | null;
  feedback: Record<string, unknown> | null;
  workflow_architecture?: string | null;
  created_at: string;
}

// ============================================================================
// Task Run Integration Types (Recent Tasks with Learning Outcomes)
// ============================================================================

/** Task information from a task run */
export interface TaskRunInfo {
  id: string;
  task_name: string;
  prompt: string | null;
  task_type: string | null;
  status: string;
  sessions_count: number;
  max_sessions: number | null;
  error_message: string | null;
  summary: string | null;
  goal_achieved: boolean | null;
  remaining_work: string | null;
  created_at: string;
  updated_at: string;
  completed_at: string | null;
}

/** Learning outcome associated with a task run */
export interface TaskLearningOutcome {
  id: string;
  status: string;
  duration_secs: number | null;
  iterations: number | null;
  strategy: string | null;
  tools_used: string[] | null;
  files_modified: string[] | null;
  error_type: string | null;
  error_message: string | null;
  feedback: Record<string, unknown> | null;
  created_at: string;
}

/** Combined task run with its learning outcome */
export interface TaskRunWithOutcome {
  task: TaskRunInfo;
  learning_outcome: TaskLearningOutcome | null;
}

/** Current running task information */
export interface CurrentRunningTask {
  id: string;
  task_name: string;
  prompt: string | null;
  task_type: string;
  status: string;
  sessions_count: number;
  max_sessions: number | null;
  created_at: string;
  updated_at: string;
}

/** Learning statistics summary for dashboard display */
export interface LearningStatsSummary {
  total_tasks: number;
  success_count: number;
  failure_count: number;
  partial_count: number;
  success_rate: number;
  avg_duration_secs: number | null;
  avg_iterations: number | null;
}
