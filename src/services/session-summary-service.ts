/**
 * session-summary-service.ts
 *
 * Automatically persists structured session summaries as observations when
 * sessions end. Subscribes to SessionManager lifecycle events and posts
 * to the /observations endpoint with type "learning" and a topic key
 * based on the session name for upsert semantics.
 *
 * Engram-inspired: every completed session produces a persistent memory
 * that can inform future sessions via the observation context system.
 */

import { createLogger } from "@/lib/logger";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import {
  sessionManager,
  type SessionContext,
  type SessionStatus,
} from "./SessionManager";

const logger = createLogger("SessionSummaryService");

// ============================================================================
// Types
// ============================================================================

interface CreateObservationPayload {
  title: string;
  content: string;
  observationType: string;
  scope: string;
  topicKey?: string;
  projectId?: string;
  sessionId?: string;
  taskRunId?: string;
}

interface SessionSummaryOptions {
  /** Project ID for scoping observations (if known) */
  projectId?: string;
  /** Minimum session duration in ms to generate summary (default 30s) */
  minDurationMs?: number;
}

// ============================================================================
// Service
// ============================================================================

class SessionSummaryService {
  private unsubscribe: (() => void) | null = null;
  private projectId: string | null = null;
  private minDurationMs = 30_000;

  /**
   * Start listening for session end events.
   */
  start(options: SessionSummaryOptions = {}): void {
    if (this.unsubscribe) {
      return; // Already listening
    }

    this.projectId = options.projectId ?? null;
    this.minDurationMs = options.minDurationMs ?? 30_000;

    this.unsubscribe = sessionManager.subscribe(
      (current, previous) => {
        // Detect session end: previous was active, current is null (no active session)
        if (previous && previous.status === "active" && current === null) {
          this.persistSessionSummary(previous).catch((err) => {
            logger.error("Failed to persist session summary:", err);
          });
        }
      },
    );

    logger.info("Session summary service started");
  }

  /**
   * Stop listening for session events.
   */
  stop(): void {
    if (this.unsubscribe) {
      this.unsubscribe();
      this.unsubscribe = null;
      logger.info("Session summary service stopped");
    }
  }

  /**
   * Update the current project ID (called when project context changes).
   */
  setProjectId(projectId: string | null): void {
    this.projectId = projectId;
  }

  /**
   * Persist a session summary as an observation.
   */
  private async persistSessionSummary(session: SessionContext): Promise<void> {
    const durationMs = Date.now() - session.startedAt;

    // Skip very short sessions (likely noise)
    if (durationMs < this.minDurationMs) {
      logger.debug(
        `Skipping short session ${session.id} (${durationMs}ms < ${this.minDurationMs}ms)`,
      );
      return;
    }

    const durationStr = formatDuration(durationMs);

    // Fetch recent task run events to enrich the summary
    const eventSummary = await this.fetchEventSummary(session.actionId);

    const title = `Session: ${session.name}`;
    const lines = [
      `## Session Summary`,
      ``,
      `- **Name:** ${session.name}`,
      `- **Duration:** ${durationStr}`,
      `- **Started:** ${new Date(session.startedAt).toISOString()}`,
      `- **Session ID:** ${session.id}`,
      `- **Action ID:** ${session.actionId}`,
    ];

    if (eventSummary) {
      lines.push(``, `### Activity`, ``);
      if (eventSummary.errorCount > 0) {
        lines.push(`- **Errors:** ${eventSummary.errorCount}`);
      }
      if (eventSummary.actionCount > 0) {
        lines.push(`- **Actions executed:** ${eventSummary.actionCount}`);
      }
      if (eventSummary.stateChanges > 0) {
        lines.push(`- **State changes:** ${eventSummary.stateChanges}`);
      }
      if (eventSummary.lastError) {
        lines.push(`- **Last error:** ${eventSummary.lastError}`);
      }
    }

    const content = lines.join("\n");

    // Use a topic key based on the session name so repeated sessions
    // for the same task upsert (evolving knowledge) rather than accumulate
    const topicKey = `session/${slugify(session.name)}`;

    const payload: CreateObservationPayload = {
      title,
      content,
      observationType: "learning",
      scope: "project",
      topicKey,
      sessionId: session.id,
    };

    if (this.projectId) {
      payload.projectId = this.projectId;
    }

    try {
      const response = await tracedFetch(`${getApiBase()}/observations`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
      });

      if (response.ok) {
        const result = await response.json();
        logger.info(
          `Session summary saved as observation ${result.data?.id} (topic: ${topicKey})`,
        );
      } else if (response.status === 503) {
        // PG not available — silently skip
        logger.debug("PostgreSQL not available, skipping session summary");
      } else {
        logger.warn(`Failed to save session summary: ${response.status}`);
      }
    } catch (err) {
      // Network error — don't crash the app
      logger.debug("Could not save session summary:", err);
    }
  }

  /**
   * Fetch a summary of task run events for the given action ID.
   */
  private async fetchEventSummary(
    actionId: string,
  ): Promise<{ errorCount: number; actionCount: number; stateChanges: number; lastError: string | null } | null> {
    try {
      const response = await tracedFetch(
        `${getApiBase()}/task-runs/${actionId}/events?max_results=50`,
      );
      if (!response.ok) return null;
      const result = await response.json();
      const events: Array<{ eventType: string; eventSubtype?: string; message: string }> =
        result.data ?? [];

      let errorCount = 0;
      let actionCount = 0;
      let stateChanges = 0;
      let lastError: string | null = null;

      for (const evt of events) {
        if (evt.eventType === "action") actionCount++;
        if (evt.eventType === "state_change") stateChanges++;
        if (evt.eventSubtype === "error" || evt.eventType === "error") {
          errorCount++;
          lastError = evt.message;
        }
      }

      return { errorCount, actionCount, stateChanges, lastError };
    } catch {
      return null;
    }
  }
}

// ============================================================================
// Helpers
// ============================================================================

function formatDuration(ms: number): string {
  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = seconds % 60;
  if (minutes < 60) return `${minutes}m ${remainingSeconds}s`;
  const hours = Math.floor(minutes / 60);
  const remainingMinutes = minutes % 60;
  return `${hours}h ${remainingMinutes}m`;
}

function slugify(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "")
    .slice(0, 64);
}

// ============================================================================
// Singleton export
// ============================================================================

export const sessionSummaryService = new SessionSummaryService();
