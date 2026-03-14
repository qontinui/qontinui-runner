/**
 * AI Conversation Widget
 *
 * Dashboard widget for displaying AI conversation output.
 * Exports both full widget and summary components.
 */

export { AiConversationWidget } from "./AiConversationWidget";
export { AiConversationSummary } from "./AiConversationSummary";
export {
  useAiConversationData,
  filterEntriesBySession,
  groupEntriesBySpeaker,
} from "./useAiConversationData";
export type { AiConversationData, AiOutputEntry } from "./types";
