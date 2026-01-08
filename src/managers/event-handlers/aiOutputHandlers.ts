/**
 * AI Output Handlers
 *
 * Handles AI output streaming events:
 * - ai_output_stream: Real-time Claude output streaming
 *
 * Uses SessionManager for session tracking.
 * Findings are now parsed by the Rust backend and received via Tauri events.
 * The TauriFindingsListener bridges Rust events to FindingsTracker.
 */

import { logManager } from "../index";
import type { HandlerSetupFunction } from "./types";
import type { AiOutputStreamEventPayload } from "../../types/eventPayloads";
import { findingsTracker } from "../../services/FindingsTracker";
import { sessionManager } from "../../services/SessionManager";
import { tauriFindingsListener } from "../../services/TauriFindingsListener";

/**
 * Get the current AI action ID (for external access if needed)
 * Now delegates to SessionManager for single source of truth.
 */
export function getCurrentAiActionId(): string | null {
  const session = sessionManager.getCurrentSession();
  return session?.actionId ?? null;
}

/**
 * Reset the AI session state (for testing or cleanup)
 * Now delegates to SessionManager.
 */
export function resetAiSessionState(): void {
  const session = sessionManager.getCurrentSession();
  if (session) {
    sessionManager.endSession("cancelled");
  }
}

/**
 * Setup AI output event handlers
 */
export const setupAiOutputHandlers: HandlerSetupFunction = (context) => {
  const { eventRouter } = context;
  const unsubscribers: Array<() => void> = [];

  // Start the Tauri findings listener to receive findings from Rust backend
  // Findings are now parsed by Rust and sent via Tauri events
  tauriFindingsListener.start().catch((error) => {
    console.error("[AI_OUTPUT_HANDLER] Failed to start TauriFindingsListener:", error);
  });

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

      // Get current session from SessionManager (single source of truth)
      const currentSession = sessionManager.getCurrentSession();
      const currentActionId = currentSession?.actionId ?? null;

      // Detect new AI session by checking if action_id changed
      if (actionId && actionId !== currentActionId) {
        console.log(
          `[AI_OUTPUT_HANDLER] New AI session detected: ${actionId} (previous: ${currentActionId})`,
        );

        // Archive previous session if one exists
        if (currentSession && findingsTracker.getCurrentReport()) {
          console.log("[AI_OUTPUT_HANDLER] Archiving previous AI session");
          // Note: archiveCurrentSession is now async but we fire-and-forget here
          // to not block the streaming handler
          findingsTracker.archiveCurrentSession("completed").catch((error) => {
            console.error("[AI_OUTPUT_HANDLER] Error archiving session:", error);
          });
        }

        // Start new session via SessionManager
        const newSession = sessionManager.startSession(actionId, sessionName || "AI Analysis");

        // Start new session in FindingsTracker using the SessionManager's session ID
        findingsTracker.startNewSession(newSession.id);
        findingsTracker.startReport(sessionName || "AI Analysis", actionId);
        console.log(`[AI_OUTPUT_HANDLER] Started findings report for session: ${newSession.id}`);
      }

      logManager.addAiOutputLog(line, source, actionId, sessionId, sessionName);

      // Process AI output for findings detection
      // Only process AI responses (not user prompts or hints)
      //
      // FindingsTracker handles:
      // - [FINDING:category:severity] markers
      // - [VERIFICATION:PENDING/COMPLETED/FAILED] markers
      // - [RUNNER:RESTART] markers
      //
      // Rust backend also parses findings and sends via TauriFindingsListener events.
      if (source === "claude" || source === "ai") {
        findingsTracker.processLine(line);
      }
    }),
  );

  // Cleanup function to stop the Tauri listener
  unsubscribers.push(() => {
    tauriFindingsListener.stop().catch((error) => {
      console.error("[AI_OUTPUT_HANDLER] Failed to stop TauriFindingsListener:", error);
    });
  });

  return unsubscribers;
};
