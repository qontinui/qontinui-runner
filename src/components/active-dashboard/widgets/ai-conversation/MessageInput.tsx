/**
 * MessageInput Component
 *
 * Text input for sending user messages to an active interactive Claude session.
 * Shows send/interrupt buttons based on session state.
 */

import { useState, useCallback, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Send, StopCircle, Loader2 } from "lucide-react";
import { cn } from "../../../../lib/utils";
import { useSessionState } from "../../../../hooks/useSessionState";
import { useCurrentTaskRunId } from "../../../../contexts/TaskContext";

/**
 * MessageInput component for the AI conversation widget.
 */
export function MessageInput() {
  const taskRunId = useCurrentTaskRunId();
  const { state, canSendMessage, canInterrupt, isActive } = useSessionState(taskRunId);
  const [message, setMessage] = useState("");
  const [isSending, setIsSending] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // Auto-resize textarea
  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 120)}px`;
  }, [message]);

  const handleSend = useCallback(async () => {
    if (!message.trim() || !taskRunId || !canSendMessage) return;

    setIsSending(true);
    try {
      await invoke("send_user_message", {
        taskRunId,
        message: message.trim(),
      });
      setMessage("");
    } catch (err) {
      console.error("[MessageInput] Failed to send message:", err);
    } finally {
      setIsSending(false);
    }
  }, [message, taskRunId, canSendMessage]);

  const handleInterrupt = useCallback(async () => {
    if (!taskRunId || !canInterrupt) return;

    try {
      await invoke("interrupt_ai_session", { taskRunId });
    } catch (err) {
      console.error("[MessageInput] Failed to interrupt:", err);
    }
  }, [taskRunId, canInterrupt]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      // Enter to send (Shift+Enter for newline)
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        handleSend();
      }
    },
    [handleSend],
  );

  // Don't render if no active session
  if (!isActive) return null;

  const isProcessing = state === "processing";
  const placeholder = isProcessing
    ? "AI is processing... (message will be queued)"
    : "Send a message...";

  return (
    <div className="border-t border-border bg-background px-3 py-2">
      <div className="flex items-end gap-2">
        <textarea
          ref={textareaRef}
          value={message}
          onChange={(e) => setMessage(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={placeholder}
          disabled={!canSendMessage || isSending}
          rows={1}
          className={cn(
            "flex-1 resize-none rounded-md border border-border bg-muted/30 px-3 py-2 text-sm",
            "placeholder:text-muted-foreground/50 focus:outline-none focus:ring-1 focus:ring-ring",
            "disabled:opacity-50 disabled:cursor-not-allowed",
            "min-h-[36px] max-h-[120px]",
          )}
        />
        {canInterrupt && (
          <button
            onClick={handleInterrupt}
            title="Interrupt AI"
            className={cn(
              "flex-shrink-0 p-2 rounded-md border border-border",
              "text-destructive hover:bg-destructive/10",
              "transition-colors",
            )}
          >
            <StopCircle className="w-4 h-4" />
          </button>
        )}
        <button
          onClick={handleSend}
          disabled={!message.trim() || !canSendMessage || isSending}
          title={isProcessing ? "Send (will be queued)" : "Send message"}
          className={cn(
            "flex-shrink-0 p-2 rounded-md",
            "bg-primary text-primary-foreground",
            "hover:bg-primary/90",
            "disabled:opacity-50 disabled:cursor-not-allowed",
            "transition-colors",
          )}
        >
          {isSending ? <Loader2 className="w-4 h-4 animate-spin" /> : <Send className="w-4 h-4" />}
        </button>
      </div>
      {isProcessing && message.trim() && (
        <p className="text-[10px] text-muted-foreground/60 mt-1 px-1">
          Message will be queued and sent after the current turn completes
        </p>
      )}
    </div>
  );
}
