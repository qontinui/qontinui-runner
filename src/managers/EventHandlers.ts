/**
 * EventHandlers
 *
 * Event handler functions that process events routed through EventRouter.
 * Each handler is responsible for updating the appropriate manager/state.
 */

import { logManager, actionLogManager, windowManager, configManager } from "./index";
import { APP_VERSION } from "../lib/appInfo";
import type { EventRouter } from "./EventRouter";
import type {
  ErrorEventPayload,
  LogEventPayload,
  ImageRecognitionEventPayload,
  ActionEventPayload,
  AiOutputStreamEventPayload,
} from "../types/eventPayloads";
import type { TreeEventData } from "../types/treeEvents";
import { syncIssuesToBackend } from "../services/IssueSyncService";
import { issueTracker } from "../services/IssueTracker";
import { findingsTracker } from "../services/FindingsTracker";
// Legacy service - deprecated, use ExecutionReportingService instead
import {
  testRunReportingService,
  type TransitionData,
  type ImageRecognitionData,
} from "../services/TestRunReportingService";
// New unified execution reporting service
import {
  executionReportingService,
  type ActionExecutionCreate,
} from "../services/ExecutionReportingService";
import { RunType, RunStatus, ActionType, ActionStatus, ErrorType } from "../types/execution";

interface ExecutionContextActions {
  setPythonStatus: (status: "stopped" | "running") => void;
  setConfigLoaded: (loaded: boolean) => void;
  setExecutionActive: (active: boolean) => void;
}

/**
 * Setup event handlers for the EventRouter
 * @param eventRouter - The EventRouter instance
 * @param executionActions - Actions from ExecutionContext
 * @returns Cleanup function to unsubscribe all handlers
 */
export function setupEventHandlers(
  eventRouter: EventRouter,
  executionActions: ExecutionContextActions,
): () => void {
  const { setPythonStatus, setConfigLoaded, setExecutionActive } = executionActions;

  // Collect unsubscribe functions
  const unsubscribers: Array<() => void> = [];

  // Track current AI session to detect new sessions
  let currentAiActionId: string | null = null;

  // Handler for "ready" event
  unsubscribers.push(
    eventRouter.subscribe("ready", () => {
      console.log("[EVENT_HANDLER] ready event received");
      setPythonStatus("running");
      logManager.addLog("info", "Python executor ready");
    }),
  );

  // Handler for "config_loaded" event
  unsubscribers.push(
    eventRouter.subscribe("config_loaded", () => {
      console.log("[EVENT_HANDLER] config_loaded event received");
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
        console.log("[EVENT_HANDLER] execution_started event received");
        console.log("[EVENT_HANDLER] Full payload:", JSON.stringify(payload, null, 2));
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
            `[EVENT_HANDLER] Starting ${runType} run for project ${projectId}, workflow ${workflowName}, initial states: ${JSON.stringify(initialStateIds)}`,
          );

          // Start using new unified service
          await executionReportingService
            .startRun(
              projectId,
              runType,
              `${workflowName} - ${new Date().toISOString().slice(0, 16)}`,
              runnerMetadata,
              workflowMetadata,
            )
            .catch((error) => {
              console.error("[EVENT_HANDLER] Failed to start execution run:", error);
            });

          // Also start legacy test run for backward compatibility
          await testRunReportingService
            .startTestRun(projectId, workflowId, workflowName)
            .catch((error) => {
              console.error("[EVENT_HANDLER] Failed to start legacy test run:", error);
            });
        } else {
          console.log("[EVENT_HANDLER] No project selected, skipping execution run reporting");
        }
      },
    ),
  );

  // Handler for "execution_completed" event
  unsubscribers.push(
    eventRouter.subscribe("execution_completed", async () => {
      console.log("[EVENT_HANDLER] execution_completed event received");
      setExecutionActive(false);
      logManager.addLog("success", "Execution completed successfully");

      // Complete execution run using new unified service (success)
      if (executionReportingService.isActive) {
        console.log("[EVENT_HANDLER] Completing execution run (success)");
        await executionReportingService.completeRun(RunStatus.COMPLETED).catch((error) => {
          console.error("[EVENT_HANDLER] Failed to complete execution run:", error);
        });
      }

      // Complete legacy test run reporting (success) for backward compatibility
      if (testRunReportingService.isActive) {
        console.log("[EVENT_HANDLER] Completing legacy test run (success)");
        await testRunReportingService.completeTestRun(true).catch((error) => {
          console.error("[EVENT_HANDLER] Failed to complete legacy test run:", error);
        });
      }

      // Restore window if it was auto-minimized
      windowManager.restoreIfMinimized();

      // Sync any detected issues to the web backend
      const issueCount = issueTracker.count;
      if (issueCount > 0) {
        console.log(`[EVENT_HANDLER] Syncing ${issueCount} issues to web backend`);
        syncIssuesToBackend()
          .then((result) => {
            if (result.errors.length > 0) {
              console.warn("[EVENT_HANDLER] Issue sync had errors:", result.errors);
              logManager.addLog(
                "warning",
                `Issue sync completed with errors: ${result.errors.join(", ")}`,
              );
            } else if (result.synced > 0 || result.updated > 0) {
              logManager.addLog(
                "info",
                `Synced ${result.synced} new, ${result.updated} updated issues to cloud`,
              );
            }
          })
          .catch((error) => {
            console.error("[EVENT_HANDLER] Failed to sync issues:", error);
            // Don't show error to user - sync is best-effort
          });
      }

      // Archive findings from the session to disk for persistence
      const findingsCount = findingsTracker.count;
      if (findingsCount > 0) {
        console.log(`[EVENT_HANDLER] Archiving ${findingsCount} findings from session`);
        findingsTracker.archiveCurrentSession("completed");
      }
    }),
  );

  // Handler for "execution_failed" event (if execution fails)
  unsubscribers.push(
    eventRouter.subscribe(
      "execution_failed",
      async (payload: { data?: { error_message?: string } }) => {
        console.log("[EVENT_HANDLER] execution_failed event received");
        setExecutionActive(false);

        const errorMessage = payload?.data?.error_message;

        // Complete execution run using new unified service (failure)
        if (executionReportingService.isActive) {
          console.log("[EVENT_HANDLER] Completing execution run (failed)");
          await executionReportingService
            .completeRun(RunStatus.FAILED, undefined, undefined, errorMessage)
            .catch((error) => {
              console.error("[EVENT_HANDLER] Failed to complete execution run:", error);
            });
        }

        // Complete legacy test run reporting (failure) for backward compatibility
        if (testRunReportingService.isActive) {
          console.log("[EVENT_HANDLER] Completing legacy test run (failed)");
          await testRunReportingService.completeTestRun(false).catch((error) => {
            console.error("[EVENT_HANDLER] Failed to complete legacy test run:", error);
          });
        }

        // Archive findings from the session (even on failure)
        const findingsCount = findingsTracker.count;
        if (findingsCount > 0) {
          console.log(`[EVENT_HANDLER] Archiving ${findingsCount} findings from failed session`);
          findingsTracker.archiveCurrentSession("failed");
        }
      },
    ),
  );

  // Handler for "error" event
  unsubscribers.push(
    eventRouter.subscribe("error", (payload: ErrorEventPayload) => {
      console.log("[EVENT_HANDLER] error event received:", payload.data);
      const errorMessage = payload.data?.message || "Unknown error occurred";
      logManager.addLog("error", errorMessage);
    }),
  );

  // Handler for "log" event
  unsubscribers.push(
    eventRouter.subscribe("log", (payload: LogEventPayload) => {
      console.log("[EVENT_HANDLER] log event received");
      const levelStr = payload.data?.level || "info";
      const message = payload.data?.message || "";

      // Type guard to ensure level is a valid log level
      const validLevels = ["info", "warning", "error", "debug", "success"] as const;
      type ValidLevel = (typeof validLevels)[number];
      const level: ValidLevel = validLevels.includes(levelStr as ValidLevel)
        ? (levelStr as ValidLevel)
        : "info";

      logManager.addLog(level, message);
    }),
  );

  // Handler for "tree_event" event
  unsubscribers.push(
    eventRouter.subscribe("tree_event", (payload) => {
      console.log("[EVENT_HANDLER] tree_event received, triggering action log refresh");
      actionLogManager.triggerRefresh();

      // Report completed actions to both new and legacy services
      // NOTE: Tree event data is at the top level of the payload, not nested under "data"
      // The Rust backend sends: { type: "tree_event", event_type: "...", node: {...}, ... }
      const treeEventData = payload as unknown as TreeEventData | undefined;
      if (treeEventData) {
        const eventData = treeEventData;
        const eventType = eventData.event_type;

        // Report on action completion (success or failure)
        if ((eventType === "action_completed" || eventType === "action_failed") && eventData.node) {
          const node = eventData.node;
          const metadata = (node.metadata || {}) as Record<string, unknown>;
          const stateContext = metadata.state_context as
            | { active_before?: string[]; active_after?: string[] }
            | undefined;

          // Calculate timestamps
          const durationMs = node.duration ? Math.round(node.duration * 1000) : 0;
          const completedAt = new Date(node.timestamp * 1000);
          const startedAt = new Date(completedAt.getTime() - durationMs);

          // Extract confidence from runtime (typed) or metadata (unknown)
          // Moved outside conditional blocks so both services can use it
          const runtime = metadata.runtime as Record<string, unknown> | undefined;
          const confidenceFromRuntime = runtime?.confidence;
          const confidenceFromMeta = metadata.confidence;
          const confidenceScore =
            typeof confidenceFromRuntime === "number"
              ? confidenceFromRuntime
              : typeof confidenceFromMeta === "number"
                ? confidenceFromMeta
                : undefined;

          // Report to new unified execution service
          if (executionReportingService.isActive) {
            // Map node type to ActionType (using node_type and action_type from metadata)
            let actionType: ActionType = ActionType.CUSTOM;
            const nodeTypeStr = (node.node_type || "").toLowerCase();
            const actionTypeFromMeta = metadata.action_type as string | undefined;
            const actionTypeStr =
              typeof actionTypeFromMeta === "string" ? actionTypeFromMeta.toLowerCase() : "";

            // First check action_type from metadata, then fall back to node_type
            const typeToCheck = actionTypeStr || nodeTypeStr;
            if (typeToCheck.includes("find")) actionType = ActionType.FIND;
            else if (typeToCheck.includes("click")) actionType = ActionType.CLICK;
            else if (typeToCheck.includes("type")) actionType = ActionType.TYPE;
            else if (typeToCheck.includes("wait")) actionType = ActionType.WAIT;
            else if (typeToCheck.includes("transition")) actionType = ActionType.TRANSITION;
            else if (typeToCheck.includes("go_to_state")) actionType = ActionType.GO_TO_STATE;

            // Map node status to ActionStatus
            const actionStatus: ActionStatus =
              node.status === "success" ? ActionStatus.SUCCESS : ActionStatus.FAILED;

            const actionExecution: ActionExecutionCreate = {
              sequence_number: executionReportingService.getNextActionSequenceNumber(),
              action_type: actionType,
              action_name: node.name || "Unknown Action",
              status: actionStatus,
              started_at: startedAt.toISOString(),
              completed_at: completedAt.toISOString(),
              duration_ms: durationMs,
              from_state: stateContext?.active_before?.[0],
              to_state: stateContext?.active_after?.[0],
              active_states: stateContext?.active_after,
              confidence_score: confidenceScore,
              error_message: node.error,
              error_type: node.error ? ErrorType.OTHER : undefined,
              screenshot_id: metadata.screenshot_reference as string | undefined,
              metadata: {
                node_id: node.id,
                node_type: node.node_type,
              },
            };

            // Report the action (async, don't block)
            executionReportingService.reportAction(actionExecution).catch((error) => {
              console.error("[EVENT_HANDLER] Failed to report action execution:", error);
            });
          }

          // Report to legacy test run service for backward compatibility
          if (testRunReportingService.isActive) {
            // Build transition data with new field names matching backend schema
            const transition: TransitionData = {
              sequence_number: testRunReportingService.getNextTransitionSequenceNumber(),
              transition_name: node.name || "Unknown Action",
              from_state: stateContext?.active_before?.[0] || "unknown",
              to_state: stateContext?.active_after?.[0] || "unknown",
              status: node.status === "success" ? "success" : "failed",
              started_at: startedAt.toISOString(),
              completed_at: completedAt.toISOString(),
              duration_ms: durationMs,
              error_message: node.error,
              error_type: node.error ? "other" : undefined, // Backend allows: element_not_found, timeout, assertion_failed, crash, other
              screenshot_id: metadata.screenshot_reference as string | undefined,
              metadata: {
                node_id: node.id,
                actions_executed: 1,
                confidence_score: confidenceScore,
              },
            };

            // Report the transition (async, don't block)
            testRunReportingService.reportTransition(transition).catch((error) => {
              console.error("[EVENT_HANDLER] Failed to report legacy transition:", error);
            });
          }
        }
      }
    }),
  );

  // Handler for "image_recognition" event
  unsubscribers.push(
    eventRouter.subscribe("image_recognition", (payload: ImageRecognitionEventPayload) => {
      console.log("[EVENT_HANDLER] image_recognition event received");
      const data = payload.data;

      if (!data) {
        console.warn("[EVENT_HANDLER] image_recognition event has no data");
        return;
      }

      // Delegate image recognition processing to LogManager
      logManager.processImageRecognitionData(data);

      // Report image recognition for historical storage (if test run is active)
      if (testRunReportingService.isActive) {
        // Parse location if it's a string
        let location: { x: number; y: number; width?: number; height?: number } | undefined;
        if (typeof data.location === "string") {
          try {
            location = JSON.parse(data.location);
          } catch {
            // Ignore parse errors
          }
        } else if (data.location && typeof data.location === "object") {
          location = data.location;
        }

        // Extract hierarchy data for active states
        let activeStates: string[] = [];
        if (data.hierarchy) {
          const hierarchy =
            typeof data.hierarchy === "string"
              ? (() => {
                  try {
                    return JSON.parse(data.hierarchy);
                  } catch {
                    return null;
                  }
                })()
              : data.hierarchy;
          if (hierarchy?.active_states && Array.isArray(hierarchy.active_states)) {
            activeStates = hierarchy.active_states;
          }
        }

        const recognitionData: ImageRecognitionData = {
          pattern_id: data.template_name || data.node_id || "unknown",
          pattern_name: data.template_name,
          action_type: "FIND",
          active_states: activeStates,
          success: data.found ?? false,
          match_count: data.found ? 1 : 0,
          best_match_score: data.confidence,
          match_x: location?.x,
          match_y: location?.y,
          match_width: location?.width,
          match_height: location?.height,
          result_data: {
            threshold: data.threshold,
            gap: data.gap,
            percent_off: data.percent_off,
            monitor_index: data.monitor_index,
          },
        };

        testRunReportingService.reportImageRecognition(recognitionData).catch((error) => {
          console.error("[EVENT_HANDLER] Failed to report image recognition:", error);
        });
      }
    }),
  );

  // Handler for "action_started" event
  unsubscribers.push(
    eventRouter.subscribe("action_started", (payload: ActionEventPayload) => {
      const actionType = payload.data?.action_type || payload.data?.type || "Unknown";
      logManager.addLog("debug", `Action started: ${actionType}`);
    }),
  );

  // Handler for "action_completed" event
  unsubscribers.push(
    eventRouter.subscribe("action_completed", (payload: ActionEventPayload) => {
      const actionType = payload.data?.action_type || payload.data?.type || "Unknown";
      logManager.addLog("debug", `Action completed: ${actionType}`);
    }),
  );

  // Handler for "ai_output_stream" event - streams Claude's output in real-time
  unsubscribers.push(
    eventRouter.subscribe("ai_output_stream", (payload: AiOutputStreamEventPayload) => {
      const data = payload.data;
      if (!data) {
        console.warn("[EVENT_HANDLER] ai_output_stream event has no data");
        return;
      }
      const line = data.line || "";
      const source = data.source || "ai";
      const actionId = data.action_id;

      // Detect new AI session by checking if action_id changed
      if (actionId && actionId !== currentAiActionId) {
        console.log(
          `[EVENT_HANDLER] New AI session detected: ${actionId} (previous: ${currentAiActionId})`,
        );

        // Archive previous session if one exists
        if (currentAiActionId && findingsTracker.getCurrentReport()) {
          console.log("[EVENT_HANDLER] Archiving previous AI session");
          findingsTracker.archiveCurrentSession("completed");
        }

        // Start new session and report
        currentAiActionId = actionId;
        findingsTracker.startNewSession(actionId);
        findingsTracker.startReport("AI Analysis", actionId);
        console.log(`[EVENT_HANDLER] Started findings report for session: ${actionId}`);
      }

      logManager.addAiOutputLog(line, source, actionId);
    }),
  );

  console.log("[EVENT_HANDLERS] All event handlers registered");

  // Return cleanup function that unsubscribes all handlers
  return () => {
    console.log("[EVENT_HANDLERS] Cleaning up all event handlers");
    unsubscribers.forEach((unsub) => unsub());
  };
}
