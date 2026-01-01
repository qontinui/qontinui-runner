/**
 * TestRunCompletion - Module for completing test runs
 *
 * @deprecated This module is part of the deprecated TestRunReportingService.
 * Use ExecutionReportingService for new implementations.
 */

import { invoke } from "@tauri-apps/api/core";
import type { CompleteTestRunInput, CompleteTestRunResponse, ExecutionStats } from "./types";
import { getStatesCoveredCount } from "./TestRunStats";

/**
 * Complete a test run with the given status and stats
 */
export async function completeTestRun(
  runId: string,
  success: boolean,
  stats: ExecutionStats,
): Promise<CompleteTestRunResponse> {
  console.log(
    `[TestRunCompletion] Completing test run ${runId} with status: ${success ? "completed" : "failed"}`,
  );

  const statesCovered = getStatesCoveredCount(stats);

  const input: CompleteTestRunInput = {
    run_id: runId,
    success,
    total_transitions: stats.totalTransitions,
    successful_transitions: stats.successfulTransitions,
    failed_transitions: stats.failedTransitions,
    timeout_transitions: 0,
    unique_transitions_covered: statesCovered,
    states_covered: statesCovered,
    coverage_percentage: 0, // Would need total states to calculate
    total_states: 0,
    deficiencies_found: 0,
    screenshots_captured: 0,
    duration_seconds: 0, // Will be calculated by backend from started_at/ended_at
  };

  const response = await invoke<CompleteTestRunResponse>("complete_test_run", { input });

  console.log(
    `[TestRunCompletion] Test run completed: ${response.status}, duration: ${response.duration_seconds}s`,
  );

  return response;
}
