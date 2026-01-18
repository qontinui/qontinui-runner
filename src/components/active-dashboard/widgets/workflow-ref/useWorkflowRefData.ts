/**
 * useWorkflowRefData Hook
 *
 * Fetches and manages data for the Workflow Ref widget.
 * Polls the step execution API for workflow reference data.
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import type { WorkflowRefData } from "./types";
import type { WorkflowRefExecution, StepStats } from "../shared/types";

const API_BASE = "http://localhost:9876";
const POLL_INTERVAL_MS = 2000;

/**
 * Response from the step executions API.
 */
interface StepExecutionResponse {
  success: boolean;
  data?: {
    executions: Array<{
      id: string;
      step_type: string;
      step_name: string;
      status: string;
      start_time?: number;
      end_time?: number;
      duration_ms?: number;
      error?: string;
      output?: string;
      // Workflow ref specific
      workflow_id?: string;
      workflow_name?: string;
      total_steps?: number;
      completed_steps?: number;
      sub_steps?: Array<{
        id: string;
        name: string;
        status: string;
        duration_ms?: number;
        error?: string;
      }>;
    }>;
  };
}

/**
 * Hook that provides workflow ref data for the widget.
 */
export function useWorkflowRefData(): WorkflowRefData {
  const [workflows, setWorkflows] = useState<WorkflowRefExecution[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [startTime, setStartTime] = useState<number | null>(null);
  const [elapsedTime, setElapsedTime] = useState(0);

  // Fetch workflow ref executions
  const fetchWorkflows = useCallback(async () => {
    try {
      const response = await fetch(`${API_BASE}/step-executions?type=workflow_ref`);
      if (response.ok) {
        const data: StepExecutionResponse = await response.json();
        if (data.success && data.data?.executions) {
          const workflowExecs: WorkflowRefExecution[] = data.data.executions.map((exec) => ({
            id: exec.id,
            name: exec.step_name || exec.workflow_name || "Sub-Workflow",
            status: exec.status as WorkflowRefExecution["status"],
            startTime: exec.start_time,
            endTime: exec.end_time,
            durationMs: exec.duration_ms,
            error: exec.error,
            output: exec.output,
            workflowId: exec.workflow_id || "",
            workflowName: exec.workflow_name || "Unknown",
            totalSteps: exec.total_steps || 0,
            completedSteps: exec.completed_steps || 0,
            subSteps: exec.sub_steps?.map((s) => ({
              id: s.id,
              name: s.name,
              status: s.status as WorkflowRefExecution["status"],
              durationMs: s.duration_ms,
              error: s.error,
            })),
          }));
          setWorkflows(workflowExecs);

          // Set start time from first workflow
          if (workflowExecs.length > 0 && !startTime) {
            const earliest = Math.min(
              ...workflowExecs.filter((w) => w.startTime).map((w) => w.startTime!),
            );
            if (earliest && earliest !== Infinity) {
              setStartTime(earliest);
            }
          }
        }
      }
    } catch {
      // Silently ignore - API may not be available
    } finally {
      setIsLoading(false);
    }
  }, [startTime]);

  // Initial fetch and polling
  useEffect(() => {
    fetchWorkflows();
    const interval = setInterval(fetchWorkflows, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [fetchWorkflows]);

  // Update elapsed time
  useEffect(() => {
    if (!startTime) {
      setElapsedTime(0);
      return;
    }

    const updateElapsed = () => {
      const now = Date.now();
      setElapsedTime(Math.floor((now - startTime) / 1000));
    };

    updateElapsed();
    const interval = setInterval(updateElapsed, 1000);
    return () => clearInterval(interval);
  }, [startTime]);

  // Get currently running workflow
  const currentWorkflow = useMemo(() => {
    return workflows.find((w) => w.status === "running") || null;
  }, [workflows]);

  // Calculate statistics
  const stats: StepStats = useMemo(() => {
    const total = workflows.length;
    const completed = workflows.filter(
      (w) => w.status === "success" || w.status === "failed",
    ).length;
    const successful = workflows.filter((w) => w.status === "success").length;
    const failed = workflows.filter((w) => w.status === "failed").length;
    const pending = workflows.filter(
      (w) => w.status === "pending" || w.status === "running",
    ).length;
    const successRate = completed > 0 ? (successful / completed) * 100 : 100;

    return {
      total,
      completed,
      successful,
      failed,
      pending,
      elapsedTime,
      successRate,
    };
  }, [workflows, elapsedTime]);

  return {
    workflows,
    currentWorkflow,
    stats,
    isLoading,
    error,
  };
}

export default useWorkflowRefData;
