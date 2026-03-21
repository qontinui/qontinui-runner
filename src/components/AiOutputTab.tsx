/**
 * AiOutputTab Component
 *
 * Displays streaming AI output from AI_PROMPT actions.
 * Shows the prompt followed by Claude's response in real-time.
 * Features:
 * - Session selector to navigate between different AI conversation sessions
 * - Scrollable output area for viewing all lines
 * - Fixed header that doesn't scroll
 */

import { useRef, useEffect, useState, useCallback, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Brain, MessageSquare, Loader2, Send, Square, Copy, Check } from "lucide-react";
import { getAccentColors } from "@/design-system";
// Note: Issue and findings detection is now done in EventHandlers.ts to ensure
// ALL lines are processed, not just filtered ones displayed in this component.
import { groupEntriesIntoLoops, type AiOutputLine } from "../types/aiLoop";
import { useAiTaskPolling } from "../hooks";
import { AiMessageDisplay, groupEntriesBySource } from "./shared";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

export type { AiOutputLine } from "../types/aiLoop";

interface AiOutputTabProps {
  lines: AiOutputLine[];
  onClear: () => void;
  /** Callback to add a line to the output (for hints) */
  onAddLine?: (line: Omit<AiOutputLine, "id">) => void;
  /** Optional filter to only show sessions with these IDs. If empty/undefined, shows all sessions. */
  filterSessionIds?: string[];
}

// Stable empty array for default prop to avoid creating new references on each render
const EMPTY_LINES: AiOutputLine[] = [];

// Time threshold (ms) to consider AI as "recently active"
const AI_ACTIVITY_THRESHOLD_MS = 5000;
// Polling interval for executor status
const STATUS_POLL_INTERVAL_MS = 1000;

export function AiOutputTab({
  lines = EMPTY_LINES,
  onClear: _onClear,
  onAddLine,
  filterSessionIds,
}: AiOutputTabProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const sessionListRef = useRef<HTMLDivElement>(null);
  const [isAiWorking, setIsAiWorking] = useState(false);
  const [promptInput, setPromptInput] = useState("");
  const [isSending, setIsSending] = useState(false);
  const [selectedLoopId, setSelectedLoopId] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [hintQueued, setHintQueued] = useState(false);
  const [queuedHint, setQueuedHint] = useState("");
  const lastLineTimestampRef = useRef<number>(0);

  // Drag-to-scroll state for session list
  const [isDragging, setIsDragging] = useState(false);
  const [startX, setStartX] = useState(0);
  const [scrollLeft, setScrollLeft] = useState(0);
  // Group lines into AI loops and optionally filter by session IDs
  const loops = useMemo(() => {
    const allLoops = groupEntriesIntoLoops(lines);
    // If filterSessionIds is provided and non-empty, only show matching sessions
    if (filterSessionIds && filterSessionIds.length > 0) {
      const filtered = allLoops.filter(
        (loop) => loop.sessionId && filterSessionIds.includes(loop.sessionId),
      );
      // If filtering resulted in no matches, fall back to showing all sessions
      if (filtered.length === 0) {
        return allLoops;
      }
      // Renumber filtered sessions to show "Session 1, 2, 3..." instead of original numbering
      return filtered.map((loop, index) => ({
        ...loop,
        label: `Session ${index + 1}`,
      }));
    }
    return allLoops;
  }, [lines, filterSessionIds]);

  // Get current loop (selected or most recent)
  const currentLoop = useMemo(() => {
    if (loops.length === 0) return null;
    if (selectedLoopId) {
      return loops.find((loop) => loop.id === selectedLoopId) || loops[loops.length - 1];
    }
    return loops[loops.length - 1];
  }, [loops, selectedLoopId]);

  // Auto-select the latest loop when a new one is created
  useEffect(() => {
    if (loops.length > 0) {
      const latestLoop = loops[loops.length - 1];
      // Only auto-select if we don't have a selection or the selected loop no longer exists
      if (!selectedLoopId || !loops.find((l) => l.id === selectedLoopId)) {
        setSelectedLoopId(latestLoop.id);
      }
    }
  }, [loops, selectedLoopId]);

  // Build conversation history from lines
  const buildConversationHistory = useCallback(() => {
    if (!lines || lines.length === 0) return "";

    // Group consecutive lines by source to reconstruct messages
    const messages: { role: "user" | "assistant"; content: string }[] = [];
    let currentRole: "user" | "assistant" | null = null;
    let currentContent: string[] = [];

    for (const line of lines) {
      // Skip status messages (like "AI is processing...")
      if (line.source === "status") continue;

      const role = line.source === "prompt" ? "user" : "assistant";

      if (role !== currentRole) {
        // Save previous message if exists
        if (currentRole && currentContent.length > 0) {
          messages.push({
            role: currentRole,
            content: currentContent.join("\n"),
          });
        }
        // Start new message
        currentRole = role;
        currentContent = [line.line];
      } else {
        // Continue current message
        currentContent.push(line.line);
      }
    }

    // Don't forget the last message
    if (currentRole && currentContent.length > 0) {
      messages.push({
        role: currentRole,
        content: currentContent.join("\n"),
      });
    }

    if (messages.length === 0) return "";

    // Format as conversation history
    let history = "## Previous Conversation\n\n";
    for (const msg of messages) {
      if (msg.role === "user") {
        history += `**User:**\n${msg.content}\n\n`;
      } else {
        history += `**Assistant:**\n${msg.content}\n\n`;
      }
    }
    history += "---\n\n## New Message\n\n";

    return history;
  }, [lines]);

  // Queue a hint to be included in the next AI call (when AI is working)
  const handleQueueHint = useCallback(() => {
    const trimmedHint = promptInput.trim();
    if (!trimmedHint) return;

    // Add hint to the output as a user comment
    if (onAddLine) {
      onAddLine({
        timestamp: Date.now(),
        line: trimmedHint,
        source: "user_hint",
        sessionId: currentLoop?.sessionId,
        sessionName: currentLoop?.sessionName,
      });
    }

    // Queue the hint for the next AI call
    setQueuedHint(trimmedHint);
    setHintQueued(true);
    setPromptInput("");
  }, [promptInput, onAddLine, currentLoop]);

  // AI task polling hook
  const { triggerTask } = useAiTaskPolling({
    onError: (error) => {
      console.error("AI prompt failed:", error);
    },
  });

  // Send a prompt to continue the conversation
  const handleSendPrompt = useCallback(async () => {
    const trimmedPrompt = promptInput.trim();
    if (!trimmedPrompt || isSending) return;

    setIsSending(true);
    try {
      // Include conversation history for context (sent to AI but not displayed)
      const history = buildConversationHistory();

      // Include queued hint if any
      let hintSection = "";
      if (queuedHint) {
        hintSection = `\n## User Hint (IMPORTANT - From the user watching this session)\n${queuedHint}\n\n`;
        // Clear the queued hint after including it
        setQueuedHint("");
        setHintQueued(false);
      }

      const fullPrompt = history + hintSection + trimmedPrompt;

      // Use async task triggering - output streams via Tauri events
      const task = await triggerTask({
        name: "ai-analysis",
        content: fullPrompt,
        max_sessions: 1,
        display_prompt: trimmedPrompt, // Only show the new message in UI
        timeout_seconds: 600,
      });

      if (task) {
        setPromptInput(""); // Clear input on success
      }
    } catch (error) {
      console.error("Failed to send AI prompt:", error);
    } finally {
      setIsSending(false);
    }
  }, [promptInput, isSending, buildConversationHistory, queuedHint, triggerTask]);

  // Handle keyboard shortcuts
  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        // When AI is working, queue as hint; otherwise send as prompt
        if (isAiWorking) {
          handleQueueHint();
        } else {
          handleSendPrompt();
        }
      }
    },
    [handleSendPrompt, handleQueueHint, isAiWorking],
  );

  // Stop the current AI analysis
  const handleStopAi = useCallback(async () => {
    try {
      const response = await tracedFetch(`${getApiBase()}/stop-ai-analysis`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
      });
      const result = await response.json();
      if (!result.success) {
        console.error("Failed to stop AI:", result.error);
      }
    } catch (error) {
      console.error("Failed to stop AI analysis:", error);
    }
  }, []);

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
      // First check if there are actually running tasks in the database
      // This is the authoritative source - if no tasks are running, AI is not working
      let hasRunningTasks = false;
      try {
        const response = await tracedFetch(`${getApiBase()}/task-runs/running`);
        if (response.ok) {
          const tasks = await response.json();
          hasRunningTasks = Array.isArray(tasks) && tasks.length > 0;
        }
      } catch {
        // If API is unavailable, fall back to other checks
        hasRunningTasks = true; // Assume running if we can't check
      }

      // If no running tasks in database, AI is definitely not working
      if (!hasRunningTasks) {
        setIsAiWorking(false);
        return;
      }

      // Additional checks for real-time activity indication
      const result = (await invoke("get_executor_status")) as {
        data?: { state?: string };
      };
      const state = result?.data?.state || "unknown";

      const now = Date.now();
      const timeSinceLastLine = now - lastLineTimestampRef.current;
      const hasRecentActivity = timeSinceLastLine < AI_ACTIVITY_THRESHOLD_MS;
      const isExecutorRunning = state === "Running";
      const lastLine = lines.length > 0 ? lines[lines.length - 1] : null;
      const isStreamingResponse = lastLine?.source === "claude";

      // Show as working if: has running tasks AND (recent activity OR executor running)
      setIsAiWorking(
        hasRunningTasks && (hasRecentActivity || isExecutorRunning || isStreamingResponse),
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

  // Auto-scroll to bottom when new lines arrive in current loop
  useEffect(() => {
    if (containerRef.current && currentLoop) {
      containerRef.current.scrollTop = containerRef.current.scrollHeight;
    }
  }, [currentLoop]);

  // Select a loop
  const handleSelectLoop = useCallback((loopId: string) => {
    setSelectedLoopId(loopId);
  }, []);

  // Drag-to-scroll handlers for session list
  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    if (!sessionListRef.current) return;
    setIsDragging(true);
    setStartX(e.pageX - sessionListRef.current.offsetLeft);
    setScrollLeft(sessionListRef.current.scrollLeft);
  }, []);

  const handleMouseMove = useCallback(
    (e: React.MouseEvent) => {
      if (!isDragging || !sessionListRef.current) return;
      e.preventDefault();
      const x = e.pageX - sessionListRef.current.offsetLeft;
      const walk = (x - startX) * 1.5; // Scroll speed multiplier
      sessionListRef.current.scrollLeft = scrollLeft - walk;
    },
    [isDragging, startX, scrollLeft],
  );

  const handleMouseUp = useCallback(() => {
    setIsDragging(false);
  }, []);

  const handleMouseLeave = useCallback(() => {
    setIsDragging(false);
  }, []);

  // Copy all text in current loop
  const handleCopyLoop = useCallback(async () => {
    if (!currentLoop) return;

    const allText = currentLoop.entries
      .map((entry) => {
        const prefix = entry.source === "prompt" ? "[Prompt]\n" : "";
        return prefix + entry.line;
      })
      .join("\n\n");

    try {
      await navigator.clipboard.writeText(allText);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch (err) {
      console.error("Failed to copy:", err);
    }
  }, [currentLoop]);

  // Empty state
  if (!lines || lines.length === 0) {
    return (
      <div className="flex-1 flex flex-col min-h-0">
        <div className="flex-1 flex flex-col items-center justify-center text-muted-foreground">
          <Brain className="w-12 h-12 mb-4 opacity-50" />
          <p className="text-lg font-medium">No AI Output Yet</p>
          <p className="text-sm mt-2">AI output will appear here when an AI_PROMPT action runs</p>
          <p className="text-sm mt-1">Or start a conversation below</p>
        </div>

        {/* Prompt input for starting a conversation */}
        <div className="mt-3 flex gap-2">
          <textarea
            ref={textareaRef}
            value={promptInput}
            onChange={(e) => setPromptInput(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder="Start a conversation with AI... (Enter to send, Shift+Enter for newline)"
            disabled={isSending}
            className="flex-1 px-3 py-2 bg-background border border-border rounded-lg text-sm resize-none min-h-[40px] max-h-[120px] focus:outline-hidden focus:ring-2 focus:ring-primary disabled:opacity-50 disabled:cursor-not-allowed"
            rows={1}
          />
          <button
            onClick={handleSendPrompt}
            disabled={!promptInput.trim() || isSending}
            className="px-4 py-2 bg-primary text-primary-foreground rounded-lg font-medium hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors flex items-center gap-2"
            title="Send prompt (Enter)"
          >
            {isSending ? (
              <Loader2 className="w-4 h-4 animate-spin" />
            ) : (
              <Send className="w-4 h-4" />
            )}
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="flex-1 flex flex-col min-h-0 overflow-hidden">
      {/* Fixed Header - doesn't scroll */}
      <div className="shrink-0 space-y-2 pb-2 mb-2 bg-background">
        {/* Session horizontal list and status row */}
        <div className="flex items-center gap-3">
          {/* Horizontal Session List - draggable */}
          <div
            ref={sessionListRef}
            onMouseDown={handleMouseDown}
            onMouseMove={handleMouseMove}
            onMouseUp={handleMouseUp}
            onMouseLeave={handleMouseLeave}
            role="list"
            aria-label="Session list"
            className={`flex-1 flex items-center gap-1.5 overflow-x-auto scrollbar-none ${
              isDragging ? "cursor-grabbing select-none" : "cursor-grab"
            }`}
            style={{ scrollbarWidth: "none", msOverflowStyle: "none" }}
          >
            {/* Sessions displayed with latest on left */}
            {[...loops].reverse().map((loop, reversedIndex) => {
              const isLatest = reversedIndex === 0;
              const isSelected = loop.id === selectedLoopId;
              return (
                <button
                  key={`${loop.id}-${reversedIndex}`}
                  onClick={() => !isDragging && handleSelectLoop(loop.id)}
                  className={`shrink-0 flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium transition-colors ${
                    isSelected
                      ? `${getAccentColors("purple").bg} ${getAccentColors("purple").text}`
                      : "bg-muted/50 text-muted-foreground hover:bg-muted hover:text-foreground"
                  }`}
                >
                  <Brain className="w-3 h-3" />
                  <span>{loop.label}</span>
                  {isLatest && (
                    <span
                      className={`px-1 py-0.5 text-[10px] rounded ${getAccentColors("purple").bg} ${getAccentColors("purple").text}`}
                    >
                      Latest
                    </span>
                  )}
                </button>
              );
            })}
          </div>

          {/* Status and actions */}
          <div className="shrink-0 flex items-center gap-2">
            {isAiWorking && (
              <>
                <div
                  className={`flex items-center gap-1.5 px-2 py-1 rounded-lg ${getAccentColors("emerald").bg}`}
                >
                  <Loader2 className={`w-3 h-3 animate-spin ${getAccentColors("emerald").text}`} />
                  <span className={`text-xs font-medium ${getAccentColors("emerald").text}`}>
                    Working
                  </span>
                </div>
                <button
                  onClick={handleStopAi}
                  className={`flex items-center gap-1 px-2 py-1 text-xs rounded-lg transition-colors ${getAccentColors("red").bg} ${getAccentColors("red").text} hover:opacity-80`}
                  title="Stop AI analysis"
                >
                  <Square className="w-3 h-3" />
                </button>
              </>
            )}
            <button
              onClick={handleCopyLoop}
              disabled={!currentLoop}
              className="flex items-center gap-1 px-2 py-1 text-xs text-muted-foreground hover:text-foreground rounded-lg hover:bg-muted/50 transition-colors disabled:opacity-50"
              title="Copy all text in this session"
            >
              {copied ? (
                <Check className={`w-3 h-3 ${getAccentColors("green").text}`} />
              ) : (
                <Copy className="w-3 h-3" />
              )}
            </button>
          </div>
        </div>
      </div>

      {/* Scrollable Output Container */}
      <div
        ref={containerRef}
        className="flex-1 min-h-0 overflow-auto bg-background/50 rounded-lg border border-border p-4 text-sm"
      >
        {currentLoop && (
          <AiMessageDisplay
            groups={groupEntriesBySource(currentLoop.entries)}
            mode="log"
            isAnimated
          />
        )}
      </div>

      {/* Prompt/Hint input */}
      <div className="shrink-0 mt-3 flex gap-2">
        <textarea
          ref={textareaRef}
          value={promptInput}
          onChange={(e) => {
            setPromptInput(e.target.value);
            if (hintQueued) setHintQueued(false);
          }}
          onKeyDown={handleKeyDown}
          placeholder={
            isAiWorking
              ? "Send a hint to the AI... (Enter to queue)"
              : "Continue the conversation... (Enter to send, Shift+Enter for newline)"
          }
          disabled={isSending}
          className={`flex-1 px-3 py-2 bg-background border rounded-lg text-sm resize-none min-h-[40px] max-h-[120px] focus:outline-hidden focus:ring-2 disabled:opacity-50 disabled:cursor-not-allowed ${
            isAiWorking
              ? `${getAccentColors("purple").border} focus:ring-purple-500`
              : "border-border focus:ring-primary"
          } ${hintQueued ? getAccentColors("green").border : ""}`}
          rows={1}
        />
        {isAiWorking ? (
          <button
            onClick={handleQueueHint}
            disabled={!promptInput.trim()}
            className={`px-4 py-2 rounded-lg font-medium disabled:opacity-50 disabled:cursor-not-allowed transition-colors flex items-center gap-2 ${getAccentColors("purple").bgSolid} text-white hover:opacity-90`}
            title="Queue hint for AI (Enter)"
          >
            <MessageSquare className="w-4 h-4" />
            <span>Hint</span>
          </button>
        ) : (
          <button
            onClick={handleSendPrompt}
            disabled={!promptInput.trim() || isSending}
            className="px-4 py-2 bg-primary text-primary-foreground rounded-lg font-medium hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors flex items-center gap-2"
            title="Send prompt (Enter)"
          >
            {isSending ? (
              <Loader2 className="w-4 h-4 animate-spin" />
            ) : (
              <Send className="w-4 h-4" />
            )}
          </button>
        )}
        {hintQueued && (
          <span className="text-xs text-green-400 self-center whitespace-nowrap">Hint queued</span>
        )}
      </div>
    </div>
  );
}
