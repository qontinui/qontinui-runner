/**
 * AiConversationSummary Component
 *
 * Summary widget for the AI conversation.
 * Shows thinking indicator and recent messages with enough height for 10 lines.
 */

import { Bot, Loader2, MessageSquare } from "lucide-react";
import { cn } from "@/lib/utils";
import { ScrollArea } from "@/components/ui/ScrollArea";
import type { BaseWidgetProps } from "@/types/dashboard/widget-props";
import { useAiConversationData } from "./useAiConversationData";
import { getAccentColors } from "@/design-system";

/**
 * Props for the AiConversationSummary.
 */
type AiConversationSummaryProps = BaseWidgetProps;

/**
 * AiConversationSummary component.
 * Displays AI conversation with enough height for ~10 lines of text.
 */
export function AiConversationSummary({
  isActive,
  status: _status,
  onRequestFocus,
  className,
}: AiConversationSummaryProps) {
  const { entries, isThinking, messageCount } = useAiConversationData();
  const greenColors = getAccentColors("green");

  // Get the last few messages for preview (enough to fill ~20 lines)
  const recentEntries = entries.slice(-10);

  return (
    <div
      className={cn(
        "flex flex-col gap-2 rounded-lg cursor-pointer",
        "transition-colors hover:bg-muted/50",
        isActive ? cn(greenColors.border, greenColors.bg) : "",
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
      {/* Header row */}
      <div className="flex items-center gap-3">
        {/* Icon */}
        <div
          className={cn(
            "shrink-0 w-8 h-8 rounded-full flex items-center justify-center",
            greenColors.bg,
            greenColors.text,
          )}
        >
          {isThinking ? <Loader2 className="w-4 h-4 animate-spin" /> : <Bot className="w-4 h-4" />}
        </div>

        {/* Title and status */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium">AI Conversation</span>
            {isThinking && (
              <span className={cn("text-xs animate-pulse", greenColors.text)}>Thinking...</span>
            )}
          </div>
        </div>

        {/* Message count badge */}
        {messageCount > 0 && (
          <div className="flex items-center gap-1 text-xs text-muted-foreground">
            <MessageSquare className="w-3 h-3" />
            <span>{messageCount}</span>
          </div>
        )}
      </div>

      {/* Message content area - height for ~20 lines (20 * 20px line-height = 400px) */}
      <ScrollArea className="h-[400px] w-full">
        <div className="space-y-2 pr-2">
          {recentEntries.length > 0 ? (
            recentEntries.map((entry) => (
              <div key={entry.id} className="text-xs text-muted-foreground">
                <span className="whitespace-pre-wrap break-words">
                  {entry.line.slice(0, 500)}
                  {entry.line.length > 500 ? "..." : ""}
                </span>
              </div>
            ))
          ) : (
            <p className="text-xs text-muted-foreground">No messages yet</p>
          )}
        </div>
      </ScrollArea>
    </div>
  );
}

export default AiConversationSummary;
