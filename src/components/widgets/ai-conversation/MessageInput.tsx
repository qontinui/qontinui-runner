/**
 * MessageInput Component
 *
 * Text input for sending user messages to an active interactive Claude session.
 * Always visible when a task is running, with session state indicator.
 * Shows send/interrupt buttons based on session state.
 *
 * Uses unfiltered session state events because the Rust executor's task_run_id
 * (emitted in claude-session-state events) may differ from the dashboard's
 * task run ID (from GET /task-runs/running). The event's own taskRunId is used
 * for IPC calls like send_user_message and interrupt_ai_session.
 */

import { useState, useCallback, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Send, StopCircle, Loader2 } from "lucide-react";
import { cn } from "@/lib/utils";
import { useSessionState } from "@/hooks/useSessionState";
import { useIsTaskRunning } from "@/contexts/TaskContext";

/**
 * Session state indicator dot + label.
 */
function SessionStateIndicator({ state, canSend }: { state: string | null; canSend: boolean }) {
  if (canSend || state === "ready") {
    return (
      <div className="flex items-center gap-1.5">
        <div className="w-2 h-2 rounded-full bg-green-500 animate-pulse" />
        <span className="text-[10px] text-green-400">Ready</span>
      </div>
    );
  }
  if (state === "processing" || state === "interrupting") {
    return (
      <div className="flex items-center gap-1.5">
        <div className="w-2 h-2 rounded-full bg-yellow-500 animate-pulse" />
        <span className="text-[10px] text-yellow-400">AI Processing</span>
      </div>
    );
  }
  if (state === "initializing" || state === "created") {
    return (
      <div className="flex items-center gap-1.5">
        <div className="w-2 h-2 rounded-full bg-blue-500 animate-pulse" />
        <span className="text-[10px] text-blue-400">Initializing</span>
      </div>
    );
  }
  return (
    <div className="flex items-center gap-1.5">
      <div className="w-2 h-2 rounded-full bg-red-500/60" />
      <span className="text-[10px] text-red-400/80">No active session</span>
    </div>
  );
}

/**
 * MessageInput component for the AI conversation widget.
 * Renders whenever a task is running, with state-aware enable/disable.
 */
export function MessageInput() {
  const isTaskRunning = useIsTaskRunning();
  // Don't filter by dashboard taskRunId — the Rust executor's task_run_id
  // (in claude-session-state events) may differ from the dashboard's ID.
  // The event's own taskRunId is used for IPC calls.
  const { state, taskRunId: sessionTaskRunId, canSendMessage, canInterrupt } = useSessionState();
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
    if (!message.trim() || !sessionTaskRunId || !canSendMessage) return;

    setIsSending(true);
    try {
      await invoke("send_user_message", {
        taskRunId: sessionTaskRunId,
        message: message.trim(),
      });
      setMessage("");
    } catch (err) {
      console.error("[MessageInput] Failed to send message:", err);
    } finally {
      setIsSending(false);
    }
  }, [message, sessionTaskRunId, canSendMessage]);

  const handleInterrupt = useCallback(async () => {
    if (!sessionTaskRunId || !canInterrupt) return;

    try {
      await invoke("interrupt_ai_session", { taskRunId: sessionTaskRunId });
    } catch (err) {
      console.error("[MessageInput] Failed to interrupt:", err);
    }
  }, [sessionTaskRunId, canInterrupt]);

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

  // Don't render if no task is running
  if (!isTaskRunning) return null;

  const isProcessing = state === "processing";
  const noSession = state === null;
  const placeholder = canSendMessage
    ? "Send a message to the AI..."
    : isProcessing
      ? "Type a message (will be queued)..."
      : noSession
        ? "No active AI session"
        : "Waiting for AI session...";

  return (
    <div className="border-t border-border bg-background px-3 py-2 space-y-1.5">
      <div className="flex items-center justify-between">
        <SessionStateIndicator state={state} canSend={canSendMessage} />
        {isProcessing && message.trim() && (
          <span className="text-[10px] text-muted-foreground/60 italic">
            Message will be queued
          </span>
        )}
      </div>
      <div className="flex items-end gap-2">
        <textarea
          ref={textareaRef}
          value={message}
          onChange={(e) => setMessage(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={placeholder}
          disabled={isSending || noSession}
          rows={1}
          className={cn(
            "flex-1 resize-none rounded-md border border-border bg-muted/30 px-3 py-2 text-sm",
            "placeholder:text-muted-foreground/50 focus:outline-hidden focus:ring-1 focus:ring-ring",
            "disabled:opacity-50 disabled:cursor-not-allowed",
            "min-h-[36px] max-h-[120px]",
          )}
        />
        {canInterrupt && (
          <button
            onClick={handleInterrupt}
            title="Interrupt AI"
            className={cn(
              "shrink-0 p-2 rounded-md border border-border",
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
            "shrink-0 p-2 rounded-md",
            "bg-primary text-primary-foreground",
            "hover:bg-primary/90",
            "disabled:opacity-50 disabled:cursor-not-allowed",
            "transition-colors",
          )}
        >
          {isSending ? <Loader2 className="w-4 h-4 animate-spin" /> : <Send className="w-4 h-4" />}
        </button>
      </div>
    </div>
  );
}
