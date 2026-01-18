/**
 * AiConversationSummary Component
 *
 * Compact summary widget for the AI conversation.
 * Shows thinking indicator or message count with a preview of the last message.
 */

import { Bot, Loader2, MessageSquare } from "lucide-react";
import { cn } from "../../../../lib/utils";
import type { BaseWidgetProps } from "../../../../types/dashboard/widget-props";
import { useAiConversationData } from "./useAiConversationData";
import { getAccentColors } from "@/design-system";

/**
 * Props for the AiConversationSummary.
 */
type AiConversationSummaryProps = BaseWidgetProps;

/**
 * Truncate text to a maximum length with ellipsis.
 */
function truncateText(text: string, maxLength: number): string {
  if (text.length <= maxLength) {
    return text;
  }
  return text.slice(0, maxLength - 3) + "...";
}

/**
 * AiConversationSummary component.
 * Displays compact AI conversation status for the summary row.
 */
export function AiConversationSummary({
  isActive,
  status: _status,
  onRequestFocus,
  className,
}: AiConversationSummaryProps) {
  const { isThinking, lastMessage, messageCount } = useAiConversationData();
  const greenColors = getAccentColors("green");

  return (
    <div
      className={cn(
        "flex items-center gap-3 px-3 py-2 rounded-lg border cursor-pointer",
        "transition-colors hover:bg-muted/50",
        isActive ? cn(greenColors.border, greenColors.bg) : "border-border",
        className,
      )}
      onClick={() => onRequestFocus?.()}
      role="button"
      tabIndex={0}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onRequestFocus?.();
        }
      }}
    >
      {/* Icon */}
      <div
        className={cn(
          "flex-shrink-0 w-8 h-8 rounded-full flex items-center justify-center",
          greenColors.bg,
          greenColors.text,
        )}
      >
        {isThinking ? <Loader2 className="w-4 h-4 animate-spin" /> : <Bot className="w-4 h-4" />}
      </div>

      {/* Content */}
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium">AI Conversation</span>
          {isThinking && (
            <span className={cn("text-xs animate-pulse", greenColors.text)}>Thinking...</span>
          )}
        </div>

        {/* Message preview or status */}
        {messageCount > 0 ? (
          <p className="text-xs text-muted-foreground truncate">
            {lastMessage ? truncateText(lastMessage, 60) : `${messageCount} messages`}
          </p>
        ) : (
          <p className="text-xs text-muted-foreground">No messages yet</p>
        )}
      </div>

      {/* Message count badge */}
      {messageCount > 0 && (
        <div className="flex items-center gap-1 text-xs text-muted-foreground">
          <MessageSquare className="w-3 h-3" />
          <span>{messageCount}</span>
        </div>
      )}
    </div>
  );
}

export default AiConversationSummary;
