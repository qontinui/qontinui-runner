/**
 * AiOutputTab Component
 *
 * Displays streaming AI output from TRIGGER_AI_ANALYSIS actions.
 * Shows the prompt followed by Claude's response in real-time.
 * Includes a working indicator that shows when AI is actively processing.
 */

import { useRef, useEffect, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Brain, Trash2, MessageSquare, Bot, Loader2 } from "lucide-react";

export interface AiOutputLine {
  id: string;
  timestamp: number;
  line: string;
  source: string; // "prompt" for user prompt, "claude" for AI response
  actionId?: string;
}

interface AiOutputTabProps {
  lines: AiOutputLine[];
  onClear: () => void;
}

// Time threshold (ms) to consider AI as "recently active"
const AI_ACTIVITY_THRESHOLD_MS = 5000;
// Polling interval for executor status
const STATUS_POLL_INTERVAL_MS = 1000;

export function AiOutputTab({ lines = [], onClear }: AiOutputTabProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [isAiWorking, setIsAiWorking] = useState(false);
  const lastLineTimestampRef = useRef<number>(0);

  // Track the most recent line timestamp
  useEffect(() => {
    if (lines.length > 0) {
      const lastLine = lines[lines.length - 1];
      lastLineTimestampRef.current = lastLine.timestamp;
    }
  }, [lines]);

  // Check executor status and determine if AI is working
  const checkAiStatus = useCallback(async () => {
    try {
      const result: any = await invoke("get_executor_status");
      const state = result?.data?.state || "unknown";

      // AI is considered "working" if:
      // 1. We have lines (prompt has been sent)
      // 2. The last line is from "claude" source (response is streaming)
      // 3. The executor is in "Running" state
      // 4. We received output recently (within threshold)
      const now = Date.now();
      const timeSinceLastLine = now - lastLineTimestampRef.current;
      const hasRecentActivity = timeSinceLastLine < AI_ACTIVITY_THRESHOLD_MS;
      const isExecutorRunning = state === "Running";
      const lastLine = lines.length > 0 ? lines[lines.length - 1] : null;
      const isStreamingResponse = lastLine?.source === "claude";

      setIsAiWorking(
        lines.length > 0 && isExecutorRunning && hasRecentActivity && isStreamingResponse,
      );
    } catch (error) {
      console.warn("[AiOutputTab] Failed to get executor status:", error);
      setIsAiWorking(false);
    }
  }, [lines]);

  // Poll executor status when we have lines
  useEffect(() => {
    if (lines.length === 0) {
      setIsAiWorking(false);
      return;
    }

    // Check immediately
    checkAiStatus();

    // Then poll periodically
    const interval = setInterval(checkAiStatus, STATUS_POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [lines, checkAiStatus]);

  // Auto-scroll to bottom when new lines arrive
  useEffect(() => {
    if (containerRef.current) {
      containerRef.current.scrollTop = containerRef.current.scrollHeight;
    }
  }, [lines]);

  if (!lines || lines.length === 0) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground">
        <Brain className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No AI Output Yet</p>
        <p className="text-sm mt-2">
          AI output will appear here when a TRIGGER_AI_ANALYSIS action runs
        </p>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col">
      {/* Header with status and clear button */}
      <div className="flex items-center justify-between mb-2">
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <Brain className="w-4 h-4" />
          <span>{lines.length} lines</span>
          {isAiWorking && (
            <div className="flex items-center gap-1.5 ml-2 px-2 py-0.5 bg-emerald-500/10 border border-emerald-500/30 rounded-full">
              <Loader2 className="w-3 h-3 text-emerald-400 animate-spin" />
              <span className="text-xs text-emerald-400 font-medium">
                AI Working
                <span className="inline-flex ml-0.5">
                  <span className="animate-pulse">.</span>
                  <span className="animate-pulse" style={{ animationDelay: "0.2s" }}>
                    .
                  </span>
                  <span className="animate-pulse" style={{ animationDelay: "0.4s" }}>
                    .
                  </span>
                </span>
              </span>
            </div>
          )}
        </div>
        <button
          onClick={onClear}
          className="flex items-center gap-1 px-2 py-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
          title="Clear AI output"
        >
          <Trash2 className="w-3 h-3" />
          Clear
        </button>
      </div>

      {/* Output container */}
      <div
        ref={containerRef}
        className="flex-1 overflow-auto bg-background/50 rounded-lg border border-border p-4 font-mono text-sm"
      >
        {lines.map((entry, index) => {
          const isPrompt = entry.source === "prompt";
          const prevEntry = index > 0 ? lines[index - 1] : null;
          const isFirstResponse = !isPrompt && prevEntry?.source === "prompt";

          if (isPrompt) {
            // Prompt styling - distinct block with user icon
            return (
              <div
                key={entry.id}
                className="bg-blue-500/10 border border-blue-500/30 rounded-lg p-3 mb-3"
              >
                <div className="flex items-center gap-2 text-blue-400 text-xs font-semibold mb-2">
                  <MessageSquare className="w-4 h-4" />
                  <span>PROMPT</span>
                  <span className="text-muted-foreground font-normal ml-auto">
                    {new Date(entry.timestamp).toLocaleTimeString()}
                  </span>
                </div>
                <div className="text-foreground whitespace-pre-wrap break-words">{entry.line}</div>
              </div>
            );
          }

          // Response styling
          return (
            <div key={entry.id}>
              {isFirstResponse && (
                <div className="flex items-center gap-2 text-emerald-400 text-xs font-semibold mb-2 mt-1">
                  <Bot className="w-4 h-4" />
                  <span>RESPONSE</span>
                </div>
              )}
              <div className="py-0.5 hover:bg-muted/50 transition-colors pl-6">
                <span className="text-muted-foreground text-xs mr-2">
                  {new Date(entry.timestamp).toLocaleTimeString()}
                </span>
                <span className="text-foreground whitespace-pre-wrap break-words">
                  {entry.line}
                </span>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
