/**
 * AI Session Types
 *
 * Defines types for AI conversation sessions.
 * Each session represents a distinct AI conversation exchange with a prompt and response.
 *
 * User-facing terminology:
 * - "Session" = One prompt + response exchange (internal: AiLoop for backward compat)
 * - "Run" = One execution attempt from the library
 * - "Step" = Progress checkpoint in multi-step tasks (internal: Phase)
 */

import type { AiOutputLine } from "../components/AiOutputTab";

/**
 * Represents a single AI conversation session (one prompt + response exchange)
 * Note: Interface kept as AiLoop for backward compatibility
 */
export interface AiLoop {
  /** Unique identifier for this session (derived from first entry's timestamp or actionId) */
  id: string;
  /** Human-readable label for this session (e.g., "Session 1") */
  label: string;
  /** Timestamp when this session started */
  startTime: number;
  /** Timestamp of the last entry in this session */
  endTime: number;
  /** All entries in this session */
  entries: AiOutputLine[];
  /** The initial prompt that started this session (truncated for display) */
  promptPreview: string;
  /** Number of detected issues in this session */
  issueCount: number;
  /** Whether this session is currently active (AI is still responding) */
  isActive: boolean;
  /** Run ID this session belongs to (if known) */
  sessionId?: string;
  /** Run name for display */
  sessionName?: string;
}

/**
 * Time gap (in ms) to consider as a new session if no actionId change
 * If there's more than 30 seconds between entries with no prompt, it's likely a new run
 */
export const SESSION_GAP_THRESHOLD_MS = 30000;
/** @deprecated Use SESSION_GAP_THRESHOLD_MS instead */
export const LOOP_GAP_THRESHOLD_MS = SESSION_GAP_THRESHOLD_MS;
/** @deprecated Use SESSION_GAP_THRESHOLD_MS instead */
export const TURN_GAP_THRESHOLD_MS = SESSION_GAP_THRESHOLD_MS;

/**
 * Maximum characters for prompt preview
 */
export const PROMPT_PREVIEW_MAX_LENGTH = 50;

/**
 * Default max lines to show before truncation
 */
export const DEFAULT_MAX_LINES = 20;

/**
 * Groups AI output entries into distinct sessions
 * Note: Function kept as groupEntriesIntoLoops for backward compatibility
 */
export function groupEntriesIntoLoops(entries: AiOutputLine[]): AiLoop[] {
  if (entries.length === 0) return [];

  const sessions: AiLoop[] = [];
  let currentSession: AiLoop | null = null;
  let currentActionId: string | undefined = undefined;
  let sessionCounter = 0;

  for (const entry of entries) {
    const isNewSession = shouldStartNewSession(entry, currentSession, currentActionId);

    if (isNewSession) {
      // Save current session if exists
      if (currentSession) {
        currentSession.endTime =
          currentSession.entries[currentSession.entries.length - 1]?.timestamp ||
          currentSession.startTime;
        sessions.push(currentSession);
      }

      // Start new session
      sessionCounter++;
      const promptPreview =
        entry.source === "prompt"
          ? truncateText(entry.line, PROMPT_PREVIEW_MAX_LENGTH)
          : "AI Response";

      currentSession = {
        // Use counter + timestamp to ensure unique ID (actionId can repeat across sessions)
        id: `session-${sessionCounter}-${entry.timestamp}`,
        label: `Session ${sessionCounter}`,
        startTime: entry.timestamp,
        endTime: entry.timestamp,
        entries: [entry],
        promptPreview,
        issueCount: 0,
        isActive: false,
        sessionId: entry.sessionId,
        sessionName: entry.sessionName,
      };
      currentActionId = entry.actionId;
    } else if (currentSession) {
      // Add to current session
      currentSession.entries.push(entry);
      currentSession.endTime = entry.timestamp;

      // Update prompt preview if this is a prompt and we don't have one yet
      if (entry.source === "prompt" && currentSession.promptPreview === "AI Response") {
        currentSession.promptPreview = truncateText(entry.line, PROMPT_PREVIEW_MAX_LENGTH);
      }
    }
  }

  // Don't forget the last session
  if (currentSession) {
    currentSession.endTime =
      currentSession.entries[currentSession.entries.length - 1]?.timestamp ||
      currentSession.startTime;
    sessions.push(currentSession);
  }

  // Post-process: fix any sessions that still have "AI Response" as promptPreview
  // by scanning their entries for actual prompts
  for (const session of sessions) {
    if (session.promptPreview === "AI Response") {
      // Look through all entries to find a prompt
      for (const entry of session.entries) {
        if (entry.source === "prompt" && entry.line.trim()) {
          session.promptPreview = truncateText(entry.line, PROMPT_PREVIEW_MAX_LENGTH);
          break;
        }
      }
    }
  }

  return sessions;
}

/**
 * Determines if a new session should be started based on the entry
 *
 * A new session is started ONLY when:
 * 1. It's the first entry (no current session)
 * 2. The actionId changes (indicates a new AI run was started)
 *
 * Manual prompts typed in the chat DO NOT create new sessions - they
 * continue the current conversation/session.
 */
function shouldStartNewSession(
  entry: AiOutputLine,
  currentSession: AiLoop | null,
  currentActionId: string | undefined,
): boolean {
  // First entry always starts a session
  if (!currentSession) return true;

  // Different actionId means new session (new AI run was started)
  if (entry.actionId && entry.actionId !== currentActionId) return true;

  // Manual prompts and responses stay in the current session
  // Only actionId changes trigger new sessions
  return false;
}

/**
 * Truncates text to a maximum length with ellipsis
 */
function truncateText(text: string, maxLength: number): string {
  const cleaned = text.replace(/\n/g, " ").trim();
  if (cleaned.length <= maxLength) return cleaned;
  return cleaned.substring(0, maxLength - 3) + "...";
}

/**
 * Counts the number of lines in a string
 */
export function countLines(text: string): number {
  return text.split("\n").length;
}

/**
 * Truncates multiline text to max lines
 */
export function truncateLines(
  text: string,
  maxLines: number,
): { truncated: string; isTruncated: boolean; totalLines: number } {
  const lines = text.split("\n");
  const totalLines = lines.length;
  if (totalLines <= maxLines) {
    return { truncated: text, isTruncated: false, totalLines };
  }
  return {
    truncated: lines.slice(0, maxLines).join("\n"),
    isTruncated: true,
    totalLines,
  };
}
