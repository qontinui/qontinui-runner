/**
 * AI Conversation Widget Types
 *
 * Type definitions for the AI Conversation dashboard widget.
 * These types represent the data structure for AI conversation display.
 */

import type { AiOutputEntry } from "@/managers/logging/LogStore";

/**
 * Data structure for the AI Conversation widget.
 */
export interface AiConversationData {
  /** All AI output entries from the log store */
  entries: AiOutputEntry[];
  /** Current session ID if one is active */
  currentSession: string | null;
  /** Whether the AI is currently thinking/processing */
  isThinking: boolean;
  /** The last message content (truncated for summary) */
  lastMessage: string | null;
  /** Total number of messages */
  messageCount: number;
}

/**
 * Re-export AiOutputEntry for convenience.
 * This avoids consumers needing to import from multiple locations.
 */
export type { AiOutputEntry };
