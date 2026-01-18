/**
 * Learning Service
 *
 * Provides Tauri invoke calls for the Learning Insights Dashboard.
 * Wraps the learning Tauri commands from src-tauri/src/commands/learning.rs
 */

import { invoke } from "@tauri-apps/api/core";
import type {
  LearningSummary,
  Pattern,
  Insight,
  AnalysisResult,
  Feedback,
  TaskOutcome,
  LearningDashboardData,
  LearningOutcomeFilter,
  PaginatedResult,
  LearningStatsByDateRange,
  LearningOutcomeRecord,
  TaskRunWithOutcome,
  CurrentRunningTask,
  LearningStatsSummary,
} from "../types/learning";

/**
 * Service for accessing learning insights data via Tauri commands.
 */
export const learningService = {
  // ===========================================================================
  // Summary & Analysis
  // ===========================================================================

  /**
   * Get the learning summary statistics.
   * @returns Summary including total tasks, success rate, patterns count, etc.
   */
  async getLearnningSummary(): Promise<LearningSummary> {
    return invoke("get_learning_summary");
  },

  /**
   * Get all identified patterns.
   * @returns Array of patterns with confidence scores and recommendations.
   */
  async getLearningPatterns(): Promise<Pattern[]> {
    return invoke("get_learning_patterns");
  },

  /**
   * Get all generated insights.
   * @returns Array of insights with categories and recommendations.
   */
  async getLearningInsights(): Promise<Insight[]> {
    return invoke("get_learning_insights");
  },

  /**
   * Run full pattern analysis and return results.
   * @returns Complete analysis including patterns, insights, and trends.
   */
  async analyzeLearningData(): Promise<AnalysisResult> {
    return invoke("analyze_learning_data");
  },

  /**
   * Get all learning dashboard data in one call.
   * @returns Combined summary, analysis, patterns, and insights.
   */
  async getLearningDashboardData(): Promise<LearningDashboardData> {
    return invoke("get_learning_dashboard_data");
  },

  // ===========================================================================
  // Feedback & Context
  // ===========================================================================

  /**
   * Get feedback applicable to a specific context.
   * @param context - Context string to match against feedback conditions.
   * @returns Array of applicable feedback items.
   */
  async getFeedbackForContext(context: string): Promise<Feedback[]> {
    return invoke("get_feedback_for_context", { context });
  },

  /**
   * Get the best performing strategy.
   * @returns Tuple of [strategy_name, success_rate] or null if none.
   */
  async getBestStrategy(): Promise<[string, number] | null> {
    return invoke("get_best_strategy");
  },

  // ===========================================================================
  // Recording & Data Management
  // ===========================================================================

  /**
   * Record a task outcome for learning.
   * @param outcome - The task outcome to record.
   */
  async recordTaskOutcome(outcome: TaskOutcome): Promise<void> {
    return invoke("record_task_outcome", { outcome });
  },

  /**
   * Export learning data as JSON for persistence.
   * @returns JSON string of all learning data.
   */
  async exportLearningData(): Promise<string> {
    return invoke("export_learning_data");
  },

  /**
   * Import learning data from JSON.
   * @param json - JSON string of learning data to import.
   */
  async importLearningData(json: string): Promise<void> {
    return invoke("import_learning_data", { json });
  },

  /**
   * Clear all learning data (for testing/reset).
   */
  async clearLearningData(): Promise<void> {
    return invoke("clear_learning_data");
  },

  /**
   * Add sample learning data for demonstration/testing.
   */
  async addSampleLearningData(): Promise<void> {
    return invoke("add_sample_learning_data");
  },

  // ===========================================================================
  // Enhanced Queries (Filtering, Pagination, Date Ranges)
  // ===========================================================================

  /**
   * Get learning outcomes with optional filtering.
   * @param filter - Filter options (status, strategy, since, limit).
   * @returns Array of filtered learning outcome records.
   */
  async getLearningOutcomesFiltered(
    filter: LearningOutcomeFilter = {},
  ): Promise<LearningOutcomeRecord[]> {
    return invoke("get_learning_outcomes_filtered", { filter });
  },

  /**
   * Get learning outcomes with pagination.
   * @param offset - Number of records to skip.
   * @param limit - Maximum number of records to return.
   * @returns Paginated result with items, total count, offset, and limit.
   */
  async getLearningOutcomesPaginated(
    offset: number,
    limit: number,
  ): Promise<PaginatedResult<LearningOutcomeRecord[]>> {
    return invoke("get_learning_outcomes_paginated", { offset, limit });
  },

  /**
   * Get learning statistics for a date range.
   * @param start - Start date (ISO 8601 format).
   * @param end - End date (ISO 8601 format).
   * @returns Statistics including counts by status, strategy, and averages.
   */
  async getLearningStatsByDateRange(start: string, end: string): Promise<LearningStatsByDateRange> {
    return invoke("get_learning_stats_by_date_range", { start, end });
  },

  /**
   * Get total count of learning outcomes.
   * @returns Total number of learning outcome records.
   */
  async getLearningOutcomesCount(): Promise<number> {
    return invoke("get_learning_outcomes_count");
  },

  // ===========================================================================
  // Task Run Integration (Recent Tasks with Learning Outcomes)
  // ===========================================================================

  /**
   * Get recent task runs with their learning outcomes joined.
   * @param limit - Maximum number of tasks to return (default: 10).
   * @returns Array of task runs with their learning outcomes.
   */
  async getRecentTasksWithOutcomes(limit?: number): Promise<TaskRunWithOutcome[]> {
    return invoke("get_recent_tasks_with_outcomes", { limit });
  },

  /**
   * Get the current running task (if any).
   * @returns The current running task or null if none.
   */
  async getCurrentRunningTask(): Promise<CurrentRunningTask | null> {
    return invoke("get_current_running_task");
  },

  /**
   * Get the most recent task ID that has checkpoints.
   * @returns The task ID or null if no tasks have checkpoints.
   */
  async getMostRecentTaskWithCheckpoints(): Promise<string | null> {
    return invoke("get_most_recent_task_with_checkpoints");
  },

  /**
   * Get learning statistics summary for dashboard display.
   * @returns Summary statistics including success rate, avg duration, etc.
   */
  async getLearningStatsSummary(): Promise<LearningStatsSummary> {
    return invoke("get_learning_stats_summary");
  },
};
