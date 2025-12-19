/**
 * AI Loop Types
 *
 * Defines types for AI conversation loops/sessions.
 * Each loop represents a distinct AI conversation with a prompt and response.
 */

import type { AiOutputLine } from "../components/AiOutputTab";

/**
 * Represents a single AI conversation loop
 */
export interface AiLoop {
  /** Unique identifier for this loop (derived from first entry's timestamp or actionId) */
  id: string;
  /** Human-readable label for this loop */
  label: string;
  /** Timestamp when this loop started */
  startTime: number;
  /** Timestamp of the last entry in this loop */
  endTime: number;
  /** All entries in this loop */
  entries: AiOutputLine[];
  /** The initial prompt that started this loop (truncated for display) */
  promptPreview: string;
  /** Number of detected issues in this loop */
  issueCount: number;
  /** Whether this loop is currently active (AI is still responding) */
  isActive: boolean;
}

/**
 * Time gap (in ms) to consider as a new loop if no actionId change
 * If there's more than 30 seconds between entries with no prompt, it's likely a new session
 */
export const LOOP_GAP_THRESHOLD_MS = 30000;

/**
 * Maximum characters for prompt preview
 */
export const PROMPT_PREVIEW_MAX_LENGTH = 50;

/**
 * Default max lines to show before truncation
 */
export const DEFAULT_MAX_LINES = 20;

/**
 * Groups AI output entries into distinct loops
 */
export function groupEntriesIntoLoops(entries: AiOutputLine[]): AiLoop[] {
  if (entries.length === 0) return [];

  const loops: AiLoop[] = [];
  let currentLoop: AiLoop | null = null;
  let currentActionId: string | undefined = undefined;
  let loopCounter = 0;

  for (const entry of entries) {
    const isNewLoop = shouldStartNewLoop(entry, currentLoop, currentActionId);

    if (isNewLoop) {
      // Save current loop if exists
      if (currentLoop) {
        currentLoop.endTime = currentLoop.entries[currentLoop.entries.length - 1]?.timestamp || currentLoop.startTime;
        loops.push(currentLoop);
      }

      // Start new loop
      loopCounter++;
      const promptPreview = entry.source === "prompt"
        ? truncateText(entry.line, PROMPT_PREVIEW_MAX_LENGTH)
        : "AI Response";

      currentLoop = {
        id: entry.actionId || `loop-${entry.timestamp}`,
        label: `Loop ${loopCounter}`,
        startTime: entry.timestamp,
        endTime: entry.timestamp,
        entries: [entry],
        promptPreview,
        issueCount: 0,
        isActive: false,
      };
      currentActionId = entry.actionId;
    } else if (currentLoop) {
      // Add to current loop
      currentLoop.entries.push(entry);
      currentLoop.endTime = entry.timestamp;

      // Update prompt preview if this is a prompt and we don't have one yet
      if (entry.source === "prompt" && currentLoop.promptPreview === "AI Response") {
        currentLoop.promptPreview = truncateText(entry.line, PROMPT_PREVIEW_MAX_LENGTH);
      }
    }
  }

  // Don't forget the last loop
  if (currentLoop) {
    currentLoop.endTime = currentLoop.entries[currentLoop.entries.length - 1]?.timestamp || currentLoop.startTime;
    loops.push(currentLoop);
  }

  return loops;
}

/**
 * Determines if a new loop should be started based on the entry
 *
 * A new loop is started ONLY when:
 * 1. It's the first entry (no current loop)
 * 2. The actionId changes (indicates "Run AI Loop" was clicked)
 *
 * Manual prompts typed in the chat DO NOT create new loops - they
 * continue the current conversation/loop.
 */
function shouldStartNewLoop(
  entry: AiOutputLine,
  currentLoop: AiLoop | null,
  currentActionId: string | undefined
): boolean {
  // First entry always starts a loop
  if (!currentLoop) return true;

  // Different actionId means new loop (Run AI Loop was clicked)
  if (entry.actionId && entry.actionId !== currentActionId) return true;

  // Manual prompts and responses stay in the current loop
  // Only actionId changes trigger new loops
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
export function truncateLines(text: string, maxLines: number): { truncated: string; isTruncated: boolean; totalLines: number } {
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
