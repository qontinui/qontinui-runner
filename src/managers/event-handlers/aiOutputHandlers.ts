/**
 * AI Output Handlers
 *
 * Handles AI output streaming events:
 * - ai_output_stream: Real-time Claude output streaming
 *
 * Also tracks AI sessions and processes output for findings/issues detection.
 */

import { logManager } from "../index";
import type { HandlerContext, HandlerSetupFunction } from "./types";
import type { AiOutputStreamEventPayload } from "../../types/eventPayloads";
import { issueTracker } from "../../services/IssueTracker";
import { findingsTracker } from "../../services/FindingsTracker";

/**
 * State for tracking current AI session
 */
let currentAiActionId: string | null = null;

/**
 * Get the current AI action ID (for external access if needed)
 */
export function getCurrentAiActionId(): string | null {
  return currentAiActionId;
}

/**
 * Reset the AI session state (for testing or cleanup)
 */
export function resetAiSessionState(): void {
  currentAiActionId = null;
}

/**
 * Setup AI output event handlers
 */
export const setupAiOutputHandlers: HandlerSetupFunction = (context) => {
  const { eventRouter } = context;
  const unsubscribers: Array<() => void> = [];

  // Handler for "ai_output_stream" event - streams Claude's output in real-time
  unsubscribers.push(
    eventRouter.subscribe("ai_output_stream", (payload: AiOutputStreamEventPayload) => {
      const data = payload.data;
      if (!data) {
        console.warn("[AI_OUTPUT_HANDLER] ai_output_stream event has no data");
        return;
      }
      const line = data.line || "";
      const source = data.source || "ai";
      const actionId = data.action_id;
      const sessionId = data.session_id;
      const sessionName = data.session_name;

      // Detect new AI session by checking if action_id changed
      if (actionId && actionId !== currentAiActionId) {
        console.log(
          `[AI_OUTPUT_HANDLER] New AI session detected: ${actionId} (previous: ${currentAiActionId})`,
        );

        // Archive previous session if one exists
        if (currentAiActionId && findingsTracker.getCurrentReport()) {
          console.log("[AI_OUTPUT_HANDLER] Archiving previous AI session");
          findingsTracker.archiveCurrentSession("completed");
        }

        // Start new session and report
        currentAiActionId = actionId;
        findingsTracker.startNewSession(actionId);
        findingsTracker.startReport("AI Analysis", actionId);
        console.log(`[AI_OUTPUT_HANDLER] Started findings report for session: ${actionId}`);
      }

      logManager.addAiOutputLog(line, source, actionId, sessionId, sessionName);

      // Process AI output for findings and issues detection
      // Only process AI responses (not user prompts or hints)
      if (source === "claude" || source === "ai") {
        // Process through IssueTracker for legacy issue detection
        issueTracker.processLine(line);
        // Process through FindingsTracker for categorized findings detection
        findingsTracker.processLine(line);
      }
    }),
  );

  return unsubscribers;
};
