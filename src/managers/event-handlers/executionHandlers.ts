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

import { createLogger } from "@/lib/logger";
import { logManager } from "../LogManager";

const logger = createLogger("ExecutionHandler");
import { windowManager } from "../WindowManager";
import { configManager } from "../ConfigManager";
import { APP_VERSION } from "../../lib/appInfo";
import type { HandlerSetupFunction } from "./types";
import type { EventPayload } from "../../types/eventPayloads";
import { findingsTracker } from "../../services/FindingsTracker";
import { executionReportingService } from "../../services/ExecutionReportingService";
import { agenticMetricsService } from "../../services/agentic-metrics-service";
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
      logger.debug("ready event received");
      setPythonStatus("running");
      logManager.addLog("info", "Python executor ready");
    }),
  );

  // Handler for "config_loaded" event
  unsubscribers.push(
    eventRouter.subscribe("config_loaded", () => {
      logger.debug("config_loaded event received");
      setConfigLoaded(true);
      logManager.addLog("info", "Configuration loaded successfully");
    }),
  );

  // Handler for "execution_started" event
  unsubscribers.push(
    eventRouter.subscribe("execution_started", async (payload: EventPayload) => {
      logger.debug("execution_started event received");
      setExecutionActive(true);

      // Start execution run reporting using the new unified service
      const projectId = configManager.getProjectId();
      if (projectId) {
        const data = payload?.data as
          | {
              workflow_id?: string;
              workflow_name?: string;
              run_type?: string;
              initial_state_ids?: string[];
            }
          | undefined;
        const workflowId = data?.workflow_id || `workflow-${Date.now()}`;
        const workflowName = data?.workflow_name || "Workflow Execution";
        const runTypeStr = data?.run_type || "live_automation";
        const initialStateIds = data?.initial_state_ids || [];

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

        logger.debug(
          `Starting ${runType} run for project ${projectId}, workflow ${workflowName}, initial states: ${JSON.stringify(initialStateIds)}`,
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
        logger.debug("No project selected, skipping execution run reporting");
      }
    }),
  );

  // Handler for "execution_completed" event
  unsubscribers.push(
    eventRouter.subscribe("execution_completed", async () => {
      logger.debug("execution_completed event received");
      setExecutionActive(false);
      logManager.addLog("success", "Execution completed successfully");

      // Complete execution run (success)
      if (executionReportingService.isActive) {
        const capturedRunId = executionReportingService.currentRunId;
        logger.debug("Completing execution run (success)");
        await executionReportingService.completeRun(RunStatus.COMPLETED).catch((error) => {
          console.error("[EXECUTION_HANDLER] Failed to complete execution run:", error);
        });

        // Push agentic scores to backend after run completes
        if (capturedRunId) {
          agenticMetricsService
            .pushLatestScoresToBackend(capturedRunId, "run")
            .catch((err) => console.warn("Failed to push scores:", err));
        }
      }

      // Restore window if it was auto-minimized
      windowManager.restoreIfMinimized();

      // Archive findings from the session to disk for persistence
      const findingsCount = findingsTracker.count;
      if (findingsCount > 0) {
        logger.debug(`Archiving ${findingsCount} findings from session`);
        findingsTracker.archiveCurrentSession("completed").catch((error) => {
          console.error("[EXECUTION_HANDLER] Failed to archive session:", error);
        });
      }
    }),
  );

  // Handler for "execution_failed" event (if execution fails)
  unsubscribers.push(
    eventRouter.subscribe("execution_failed", async (payload: EventPayload) => {
      logger.debug("execution_failed event received");
      setExecutionActive(false);

      const data = payload?.data as { error_message?: string } | undefined;
      const errorMessage = data?.error_message;

      // Complete execution run (failure)
      if (executionReportingService.isActive) {
        const capturedRunId = executionReportingService.currentRunId;
        logger.debug("Completing execution run (failed)");
        await executionReportingService
          .completeRun(RunStatus.FAILED, undefined, undefined, errorMessage)
          .catch((error) => {
            console.error("[EXECUTION_HANDLER] Failed to complete execution run:", error);
          });

        // Push agentic scores to backend after run completes
        if (capturedRunId) {
          agenticMetricsService
            .pushLatestScoresToBackend(capturedRunId, "run")
            .catch((err) => console.warn("Failed to push scores:", err));
        }
      }

      // Archive findings from the session (even on failure)
      const findingsCount = findingsTracker.count;
      if (findingsCount > 0) {
        logger.debug(`Archiving ${findingsCount} findings from failed session`);
        findingsTracker.archiveCurrentSession("failed").catch((error) => {
          console.error("[EXECUTION_HANDLER] Failed to archive failed session:", error);
        });
      }
    }),
  );

  return unsubscribers;
};
