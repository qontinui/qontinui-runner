/**
 * Execution Handlers
 *
 * Handles workflow execution lifecycle events:
 * - ready: Python executor is ready
 * - config_loaded: Configuration has been loaded
 * - execution_started: Workflow execution has started
 * - execution_completed: Workflow execution completed successfully
 * - execution_failed: Workflow execution failed
 */

import { logManager, windowManager, configManager } from "../index";
import { APP_VERSION } from "../../lib/appInfo";
import type { HandlerSetupFunction } from "./types";
import { findingsTracker } from "../../services/FindingsTracker";
import { executionReportingService } from "../../services/ExecutionReportingService";
import { RunType, RunStatus } from "../../types/execution";

/**
 * Setup execution-related event handlers
 */
export const setupExecutionHandlers: HandlerSetupFunction = (context) => {
  const { eventRouter, executionActions } = context;
  const { setPythonStatus, setConfigLoaded, setExecutionActive } = executionActions;
  const unsubscribers: Array<() => void> = [];

  // Handler for "ready" event
  unsubscribers.push(
    eventRouter.subscribe("ready", () => {
      console.log("[EXECUTION_HANDLER] ready event received");
      setPythonStatus("running");
      logManager.addLog("info", "Python executor ready");
    }),
  );

  // Handler for "config_loaded" event
  unsubscribers.push(
    eventRouter.subscribe("config_loaded", () => {
      console.log("[EXECUTION_HANDLER] config_loaded event received");
      setConfigLoaded(true);
      logManager.addLog("info", "Configuration loaded successfully");
    }),
  );

  // Handler for "execution_started" event
  unsubscribers.push(
    eventRouter.subscribe(
      "execution_started",
      async (payload: {
        data?: {
          workflow_id?: string;
          workflow_name?: string;
          run_type?: string;
          initial_state_ids?: string[];
        };
      }) => {
        console.log("[EXECUTION_HANDLER] execution_started event received");
        console.log("[EXECUTION_HANDLER] Full payload:", JSON.stringify(payload, null, 2));
        setExecutionActive(true);

        // Start execution run reporting using the new unified service
        const projectId = configManager.getProjectId();
        if (projectId) {
          const workflowId = payload?.data?.workflow_id || `workflow-${Date.now()}`;
          const workflowName = payload?.data?.workflow_name || "Workflow Execution";
          const runTypeStr = payload?.data?.run_type || "live_automation";
          const initialStateIds = payload?.data?.initial_state_ids || [];

          // Map run type string to enum
          let runType: RunType;
          switch (runTypeStr) {
            case "qa_test":
              runType = RunType.QA_TEST;
              break;
            case "integration_test":
              runType = RunType.INTEGRATION_TEST;
              break;
            case "recording":
              runType = RunType.RECORDING;
              break;
            case "debug":
              runType = RunType.DEBUG;
              break;
            default:
              runType = RunType.LIVE_AUTOMATION;
          }

          // Get runner metadata
          const runnerMetadata = {
            runner_version: APP_VERSION,
            os: navigator.platform || "unknown",
            hostname: "qontinui-runner",
          };

          // Get workflow metadata including initial states
          const workflowMetadata = {
            workflow_id: workflowId,
            workflow_name: workflowName,
            initial_state_ids: initialStateIds.length > 0 ? initialStateIds : undefined,
          };

          console.log(
            `[EXECUTION_HANDLER] Starting ${runType} run for project ${projectId}, workflow ${workflowName}, initial states: ${JSON.stringify(initialStateIds)}`,
          );

          // Start execution run reporting
          await executionReportingService
            .startRun(
              projectId,
              runType,
              `${workflowName} - ${new Date().toISOString().slice(0, 16)}`,
              runnerMetadata,
              workflowMetadata,
            )
            .catch((error) => {
              console.error("[EXECUTION_HANDLER] Failed to start execution run:", error);
            });
        } else {
          console.log("[EXECUTION_HANDLER] No project selected, skipping execution run reporting");
        }
      },
    ),
  );

  // Handler for "execution_completed" event
  unsubscribers.push(
    eventRouter.subscribe("execution_completed", async () => {
      console.log("[EXECUTION_HANDLER] execution_completed event received");
      setExecutionActive(false);
      logManager.addLog("success", "Execution completed successfully");

      // Complete execution run (success)
      if (executionReportingService.isActive) {
        console.log("[EXECUTION_HANDLER] Completing execution run (success)");
        await executionReportingService.completeRun(RunStatus.COMPLETED).catch((error) => {
          console.error("[EXECUTION_HANDLER] Failed to complete execution run:", error);
        });
      }

      // Restore window if it was auto-minimized
      windowManager.restoreIfMinimized();

      // Archive findings from the session to disk for persistence
      const findingsCount = findingsTracker.count;
      if (findingsCount > 0) {
        console.log(`[EXECUTION_HANDLER] Archiving ${findingsCount} findings from session`);
        findingsTracker.archiveCurrentSession("completed").catch((error) => {
          console.error("[EXECUTION_HANDLER] Failed to archive session:", error);
        });
      }
    }),
  );

  // Handler for "execution_failed" event (if execution fails)
  unsubscribers.push(
    eventRouter.subscribe(
      "execution_failed",
      async (payload: { data?: { error_message?: string } }) => {
        console.log("[EXECUTION_HANDLER] execution_failed event received");
        setExecutionActive(false);

        const errorMessage = payload?.data?.error_message;

        // Complete execution run (failure)
        if (executionReportingService.isActive) {
          console.log("[EXECUTION_HANDLER] Completing execution run (failed)");
          await executionReportingService
            .completeRun(RunStatus.FAILED, undefined, undefined, errorMessage)
            .catch((error) => {
              console.error("[EXECUTION_HANDLER] Failed to complete execution run:", error);
            });
        }

        // Archive findings from the session (even on failure)
        const findingsCount = findingsTracker.count;
        if (findingsCount > 0) {
          console.log(
            `[EXECUTION_HANDLER] Archiving ${findingsCount} findings from failed session`,
          );
          findingsTracker.archiveCurrentSession("failed").catch((error) => {
            console.error("[EXECUTION_HANDLER] Failed to archive failed session:", error);
          });
        }
      },
    ),
  );

  return unsubscribers;
};
