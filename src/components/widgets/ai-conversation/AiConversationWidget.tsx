/**
 * AiConversationWidget Component
 *
 * Full-size widget displaying the AI conversation in a chat-style interface.
 * Shows user prompts and AI responses with auto-scroll and thinking indicator.
 *
 * Note: The widget header is rendered by DashboardLayout, not by this component.
 */

import { useEffect, useMemo, useRef } from "react";
import { Bot, Loader2 } from "lucide-react";
import { cn } from "@/lib/utils";
import { ScrollArea } from "@/components/ui/ScrollArea";
import type { BaseWidgetProps } from "@/types/dashboard/widget-props";
import { useAiConversationData } from "./useAiConversationData";
import { AiMessageDisplay, groupEntriesBySource } from "@/components/shared";
import { MessageInput } from "./MessageInput";
import { getAccentColors } from "@/design-system";

/**
 * Props for the AiConversationWidget.
 */
type AiConversationWidgetProps = BaseWidgetProps;

/**
 * Thinking indicator shown when AI is processing.
 */
function ThinkingIndicator() {
  const greenColors = getAccentColors("green");
  return (
    <div className="flex gap-3 px-4 py-3">
      <div
        className={cn(
          "flex-shrink-0 w-8 h-8 rounded-full flex items-center justify-center",
          greenColors.bg,
          greenColors.text,
        )}
      >
        <Bot className="w-4 h-4" />
      </div>
      <div className="flex items-center gap-2 bg-muted/50 border border-border rounded-lg px-4 py-2">
        <Loader2 className={cn("w-4 h-4 animate-spin", greenColors.text)} />
        <span className="text-sm text-muted-foreground">AI is thinking...</span>
      </div>
    </div>
  );
}

/**
 * Empty state when no conversation exists.
 */
function EmptyState() {
  const greenColors = getAccentColors("green");
  return (
    <div className="flex-1 flex flex-col items-center justify-center text-center p-8">
      <div
        className={cn(
          "w-16 h-16 rounded-full flex items-center justify-center mb-4",
          greenColors.bg,
        )}
      >
        <Bot className={cn("w-8 h-8", greenColors.text)} />
      </div>
      <h3 className="text-lg font-medium mb-2">No AI Conversation Yet</h3>
      <p className="text-sm text-muted-foreground max-w-xs">
        The AI conversation will appear here once the workflow starts running.
      </p>
    </div>
  );
}

/**
 * AiConversationWidget component.
 * Displays full chat interface when active.
 */
export function AiConversationWidget({
  isActive,
  isSummary,
  status: _status,
  onNavigateToDetail: _onNavigateToDetail,
  onRequestFocus,
  className,
}: AiConversationWidgetProps) {
  const { entries, isThinking, messageCount: _messageCount } = useAiConversationData();
  const scrollRef = useRef<HTMLDivElement>(null);

  // Use shared grouping function for consistent behavior with AiOutputTab
  // Memoize to prevent unnecessary re-renders of child components
  const groups = useMemo(() => groupEntriesBySource(entries), [entries]);

  // Auto-scroll to bottom when new messages arrive
  useEffect(() => {
    if (scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [entries.length]);

  // If summary mode, render the compact version
  if (isSummary) {
    return null; // Summary is handled by AiConversationSummary
  }

  // Note: Header is rendered by DashboardLayout, not here
  return (
    <div
      className={cn("flex flex-col h-full", className)}
      onClick={() => !isActive && onRequestFocus?.()}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          if (!isActive) onRequestFocus?.();
        }
      }}
      role="button"
      tabIndex={0}
    >
      {/* Content */}
      <ScrollArea className="flex-1">
        <div ref={scrollRef} className="flex flex-col py-2">
          {groups.length === 0 ? (
            <EmptyState />
          ) : (
            <>
              <AiMessageDisplay groups={groups} mode="chat" />
              {isThinking && <ThinkingIndicator />}
            </>
          )}
        </div>
      </ScrollArea>

      {/* Message input for interactive sessions */}
      <MessageInput />
    </div>
  );
}

export default AiConversationWidget;
