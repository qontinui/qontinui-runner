import { useCallback } from "react";
import { getApiPort } from "@/lib/runner-api";
import type { UIBridgeRequestPayload, UIBridgeEventContext } from "./types";
import { mapTaskRunStatus } from "./utils";

/**
 * Handles: get_workflows, run_workflow, get_workflow_status
 */
export function useWorkflowEvents(
  context: Pick<UIBridgeEventContext, "bridgeRef" | "sendResponse">,
) {
  const { bridgeRef, sendResponse } = context;

  return useCallback(
    async (payload: UIBridgeRequestPayload): Promise<boolean> => {
      const { requestId, type } = payload;
      const currentBridge = bridgeRef.current;

      switch (type) {
        case "get_workflows": {
          // Return workflows from snapshot
          const snapshot = await currentBridge.createSnapshotAsync();
          await sendResponse({
            requestId,
            type,
            success: true,
            data: {
              workflows: (snapshot as unknown as Record<string, unknown>).workflows ?? [],
              timestamp: Date.now(),
            },
            timestamp: Date.now(),
          });
          return true;
        }

        case "run_workflow": {
          // Execute a workflow via the runner's unified workflow engine
          const workflowId = (payload as unknown as Record<string, unknown>).id as string;
          const runRequest = ((payload as unknown as Record<string, unknown>).request ||
            {}) as Record<string, unknown>;
          try {
            const port = getApiPort();
            const runResponse = await fetch(
              `http://127.0.0.1:${port}/unified-workflows/${encodeURIComponent(workflowId)}/run`,
              {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({
                  force_fresh_start:
                    (runRequest.params as Record<string, unknown>)?.force_fresh_start ?? false,
                }),
              },
            );
            const runResult = (await runResponse.json()) as Record<string, unknown>;
            const runResultData = (runResult.data || {}) as Record<string, unknown>;
            await sendResponse({
              requestId,
              type,
              success: (runResult.success as boolean) ?? runResponse.ok,
              data: {
                workflowId,
                runId:
                  runResultData.task_run_id || runResultData.execution_id || `run-${Date.now()}`,
                status: "running",
                steps: [],
                totalSteps: 0,
                startedAt: Date.now(),
              },
              timestamp: Date.now(),
            });
          } catch (runErr) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: String(runErr),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        case "get_workflow_status": {
          // Get workflow run status from the runner
          const statusRunId = (payload as unknown as Record<string, unknown>).runId as string;
          try {
            const port = getApiPort();
            const statusResponse = await fetch(
              `http://127.0.0.1:${port}/task-runs/${encodeURIComponent(statusRunId)}`,
            );
            if (!statusResponse.ok) {
              const errBody = (await statusResponse.json().catch(() => ({}))) as Record<
                string,
                unknown
              >;
              await sendResponse({
                requestId,
                type,
                success: false,
                error: (errBody.error as string) || `Runner returned ${statusResponse.status}`,
                timestamp: Date.now(),
              });
              return true;
            }
            const statusResult = (await statusResponse.json()) as Record<string, unknown>;
            const taskRun = (statusResult.data || statusResult) as Record<string, unknown>;
            await sendResponse({
              requestId,
              type,
              success: true,
              data: {
                workflowId: taskRun.workflow_id || "",
                runId: statusRunId,
                status: mapTaskRunStatus(taskRun.status as string),
                steps: [],
                totalSteps: (taskRun.total_steps as number) || 0,
                currentStep: taskRun.current_step,
                startedAt: taskRun.created_at
                  ? new Date(taskRun.created_at as string).getTime()
                  : Date.now(),
                completedAt: taskRun.completed_at
                  ? new Date(taskRun.completed_at as string).getTime()
                  : undefined,
              },
              timestamp: Date.now(),
            });
          } catch (statusErr) {
            await sendResponse({
              requestId,
              type,
              success: false,
              error: String(statusErr),
              timestamp: Date.now(),
            });
          }
          return true;
        }

        default:
          return false;
      }
    },
    [bridgeRef, sendResponse],
  );
}
