/**
 * AiOutputTab Component
 *
 * Displays streaming AI output from AI_PROMPT actions.
 * Shows the prompt followed by Claude's response in real-time.
 * Features:
 * - AI Loop selector to navigate between different conversation sessions
 * - Truncation of long responses with expand/collapse toggle
 * - Error summary showing detected issues
 * - Fixed header that doesn't scroll
 */

import { useRef, useEffect, useState, useCallback, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Brain,
  Trash2,
  MessageSquare,
  Bot,
  Loader2,
  Send,
  ChevronDown,
  ChevronUp,
  AlertTriangle,
  CheckCircle,
  Clock,
  ChevronRight,
  Square,
} from "lucide-react";
import { issueTracker } from "../services";
import { groupEntriesIntoLoops, DEFAULT_MAX_LINES /* type AiLoop */ } from "../types/aiLoop";
import type { DetectedIssue } from "../types/issues";

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
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const [isAiWorking, setIsAiWorking] = useState(false);
  const [promptInput, setPromptInput] = useState("");
  const [isSending, setIsSending] = useState(false);
  const [selectedLoopId, setSelectedLoopId] = useState<string | null>(null);
  const [expandedResponses, setExpandedResponses] = useState<Set<string>>(new Set());
  const [showLoopSelector, setShowLoopSelector] = useState(false);
  const [showErrorSummary, setShowErrorSummary] = useState(true);
  const lastLineTimestampRef = useRef<number>(0);
  const processedLinesRef = useRef<Set<string>>(new Set());

  // Track issues for the current session
  const [sessionIssues, setSessionIssues] = useState<DetectedIssue[]>([]);

  // Group lines into AI loops
  const loops = useMemo(() => groupEntriesIntoLoops(lines), [lines]);

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

  // Process new lines through IssueTracker for issue detection
  useEffect(() => {
    for (const line of lines) {
      // Skip already processed lines
      if (processedLinesRef.current.has(line.id)) continue;

      // Only process AI responses (claude source), not user prompts
      if (line.source === "claude") {
        issueTracker.processLine(line.line);
      }

      // Mark as processed
      processedLinesRef.current.add(line.id);
    }
  }, [lines]);

  // Subscribe to issue tracker updates
  useEffect(() => {
    const updateIssues = () => {
      setSessionIssues(issueTracker.getSessionIssues());
    };
    updateIssues();
    const unsubscribe = issueTracker.subscribe(updateIssues);
    return unsubscribe;
  }, []);

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

  // Send a prompt to continue the conversation
  const handleSendPrompt = useCallback(async () => {
    const trimmedPrompt = promptInput.trim();
    if (!trimmedPrompt || isSending) return;

    setIsSending(true);
    try {
      // Include conversation history for context (sent to AI but not displayed)
      const history = buildConversationHistory();
      const fullPrompt = history + trimmedPrompt;

      const response = await fetch("http://localhost:9876/trigger-ai-analysis", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          prompt: fullPrompt,
          display_prompt: trimmedPrompt, // Only show the new message in UI
          timeout_seconds: 600,
        }),
      });

      const result = await response.json();
      if (result.success) {
        setPromptInput(""); // Clear input on success
      } else {
        console.error("AI prompt failed:", result.error || result.data?.error);
      }
    } catch (error) {
      console.error("Failed to send AI prompt:", error);
    } finally {
      setIsSending(false);
    }
  }, [promptInput, isSending, buildConversationHistory]);

  // Handle keyboard shortcuts
  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        handleSendPrompt();
      }
    },
    [handleSendPrompt],
  );

  // Stop the current AI analysis
  const handleStopAi = useCallback(async () => {
    try {
      const response = await fetch("http://localhost:9876/stop-ai-analysis", {
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
      const result: any = await invoke("get_executor_status");
      const state = result?.data?.state || "unknown";

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

  // Auto-scroll to bottom when new lines arrive in current loop
  useEffect(() => {
    if (containerRef.current && currentLoop) {
      containerRef.current.scrollTop = containerRef.current.scrollHeight;
    }
  }, [currentLoop?.entries.length]);

  // Toggle response expansion
  const toggleResponseExpand = useCallback((entryId: string) => {
    setExpandedResponses((prev) => {
      const next = new Set(prev);
      if (next.has(entryId)) {
        next.delete(entryId);
      } else {
        next.add(entryId);
      }
      return next;
    });
  }, []);

  // Select a loop
  const handleSelectLoop = useCallback((loopId: string) => {
    setSelectedLoopId(loopId);
    setShowLoopSelector(false);
  }, []);

  // Get issue counts by status
  const issueSummary = useMemo(() => {
    const summary = {
      total: sessionIssues.length,
      detected: 0,
      inProgress: 0,
      resolved: 0,
      critical: 0,
      high: 0,
    };
    for (const issue of sessionIssues) {
      if (issue.status === "detected") summary.detected++;
      if (issue.status === "in_progress") summary.inProgress++;
      if (issue.status === "resolved") summary.resolved++;
      if (issue.severity === "critical") summary.critical++;
      if (issue.severity === "high") summary.high++;
    }
    return summary;
  }, [sessionIssues]);

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
            className="flex-1 px-3 py-2 bg-background border border-border rounded-lg text-sm resize-none min-h-[40px] max-h-[120px] focus:outline-none focus:ring-2 focus:ring-primary disabled:opacity-50 disabled:cursor-not-allowed"
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
      <div className="flex-shrink-0 space-y-2 pb-2 border-b border-border mb-2 bg-background">
        {/* Loop selector and status row */}
        <div className="flex items-center justify-between">
          {/* Loop Selector */}
          <div className="relative">
            <button
              onClick={() => setShowLoopSelector(!showLoopSelector)}
              className="flex items-center gap-2 px-3 py-1.5 bg-muted hover:bg-muted/80 rounded-lg text-sm font-medium transition-colors"
            >
              <Brain className="w-4 h-4" />
              <span>{currentLoop ? currentLoop.label : "Select Loop"}</span>
              <span className="text-xs text-muted-foreground">({loops.length} total)</span>
              <ChevronDown
                className={`w-4 h-4 transition-transform ${showLoopSelector ? "rotate-180" : ""}`}
              />
            </button>

            {/* Loop dropdown - latest first */}
            {showLoopSelector && (
              <div className="absolute top-full left-0 mt-1 w-72 bg-popover border border-border rounded-lg shadow-lg z-50 max-h-64 overflow-y-auto">
                {[...loops].reverse().map((loop) => {
                  // Find the original index for display (1-based)
                  const loopNumber = loops.findIndex((l) => l.id === loop.id) + 1;
                  return (
                    <button
                      key={loop.id}
                      onClick={() => handleSelectLoop(loop.id)}
                      className={`w-full flex items-start gap-2 px-3 py-2 text-left hover:bg-muted transition-colors ${
                        loop.id === selectedLoopId ? "bg-muted" : ""
                      }`}
                    >
                      <ChevronRight className="w-4 h-4 mt-0.5 flex-shrink-0" />
                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-2">
                          <span className="font-medium text-sm">Loop {loopNumber}</span>
                          <span className="text-xs text-muted-foreground">
                            {new Date(loop.startTime).toLocaleTimeString()}
                          </span>
                          {loopNumber === loops.length && (
                            <span className="text-xs px-1.5 py-0.5 bg-primary/20 text-primary rounded">
                              Latest
                            </span>
                          )}
                        </div>
                        <p className="text-xs text-muted-foreground truncate">
                          {loop.promptPreview}
                        </p>
                        <div className="flex items-center gap-2 mt-1 text-xs">
                          <span>{loop.entries.length} messages</span>
                        </div>
                      </div>
                    </button>
                  );
                })}
              </div>
            )}
          </div>

          {/* Status and actions */}
          <div className="flex items-center gap-3">
            {isAiWorking && (
              <>
                <div className="flex items-center gap-1.5 px-2 py-0.5 bg-emerald-500/10 border border-emerald-500/30 rounded-full">
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
                <button
                  onClick={handleStopAi}
                  className="flex items-center gap-1 px-2 py-1 text-xs bg-red-500/20 text-red-400 hover:bg-red-500/30 rounded transition-colors"
                  title="Stop AI analysis"
                >
                  <Square className="w-3 h-3" />
                  Stop
                </button>
              </>
            )}
            <span className="text-xs text-muted-foreground">
              {currentLoop?.entries.length || 0} messages
            </span>
            <button
              onClick={onClear}
              className="flex items-center gap-1 px-2 py-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
              title="Clear all AI output"
            >
              <Trash2 className="w-3 h-3" />
              Clear All
            </button>
          </div>
        </div>

        {/* Error Summary (always visible, collapsible) */}
        <div className="bg-muted/50 rounded-lg border border-border">
          <button
            onClick={() => setShowErrorSummary(!showErrorSummary)}
            className="w-full flex items-center justify-between px-3 py-2 text-sm font-medium hover:bg-muted/80 transition-colors rounded-lg"
          >
            <div className="flex items-center gap-2">
              {issueSummary.total > 0 ? (
                <AlertTriangle
                  className={`w-4 h-4 ${issueSummary.critical > 0 ? "text-red-500" : issueSummary.high > 0 ? "text-orange-500" : "text-yellow-500"}`}
                />
              ) : (
                <CheckCircle className="w-4 h-4 text-green-500" />
              )}
              <span>
                {issueSummary.total > 0
                  ? `Issues Detected: ${issueSummary.total}`
                  : "No Issues Detected"}
              </span>
            </div>
            <div className="flex items-center gap-3">
              {issueSummary.detected > 0 && (
                <span className="text-xs px-2 py-0.5 bg-red-500/20 text-red-400 rounded">
                  {issueSummary.detected} open
                </span>
              )}
              {issueSummary.inProgress > 0 && (
                <span className="text-xs px-2 py-0.5 bg-blue-500/20 text-blue-400 rounded">
                  {issueSummary.inProgress} in progress
                </span>
              )}
              {issueSummary.resolved > 0 && (
                <span className="text-xs px-2 py-0.5 bg-green-500/20 text-green-400 rounded">
                  {issueSummary.resolved} resolved
                </span>
              )}
              {showErrorSummary ? (
                <ChevronUp className="w-4 h-4" />
              ) : (
                <ChevronDown className="w-4 h-4" />
              )}
            </div>
          </button>

          {showErrorSummary && issueSummary.total > 0 && (
            <div className="px-3 pb-2 space-y-1 max-h-32 overflow-y-auto">
              {sessionIssues.slice(0, 5).map((issue) => (
                <div
                  key={issue.id}
                  className="flex items-center gap-2 text-xs py-1 border-t border-border/50"
                >
                  {issue.status === "resolved" ? (
                    <CheckCircle className="w-3 h-3 text-green-500 flex-shrink-0" />
                  ) : issue.status === "in_progress" ? (
                    <Clock className="w-3 h-3 text-blue-500 flex-shrink-0" />
                  ) : (
                    <AlertTriangle
                      className={`w-3 h-3 flex-shrink-0 ${
                        issue.severity === "critical"
                          ? "text-red-500"
                          : issue.severity === "high"
                            ? "text-orange-500"
                            : "text-yellow-500"
                      }`}
                    />
                  )}
                  <span
                    className={`px-1.5 py-0.5 rounded text-[10px] uppercase font-medium ${
                      issue.severity === "critical"
                        ? "bg-red-500/20 text-red-400"
                        : issue.severity === "high"
                          ? "bg-orange-500/20 text-orange-400"
                          : issue.severity === "medium"
                            ? "bg-yellow-500/20 text-yellow-400"
                            : "bg-gray-500/20 text-gray-400"
                    }`}
                  >
                    {issue.severity}
                  </span>
                  <span className="truncate flex-1" title={issue.title}>
                    {issue.title}
                  </span>
                  {issue.file && (
                    <span
                      className="text-muted-foreground truncate max-w-[120px]"
                      title={issue.file}
                    >
                      {issue.file.split("/").pop()}
                    </span>
                  )}
                </div>
              ))}
              {sessionIssues.length > 5 && (
                <div className="text-xs text-muted-foreground pt-1 border-t border-border/50">
                  +{sessionIssues.length - 5} more issues (see Issues tab)
                </div>
              )}
            </div>
          )}
        </div>
      </div>

      {/* Scrollable Output Container */}
      <div
        ref={containerRef}
        className="flex-1 min-h-0 overflow-auto bg-background/50 rounded-lg border border-border p-4 font-mono text-sm"
      >
        {currentLoop?.entries.map((entry, index) => {
          const isPrompt = entry.source === "prompt";
          const prevEntry = index > 0 ? currentLoop.entries[index - 1] : null;
          const isFirstResponse = !isPrompt && prevEntry?.source === "prompt";

          // Group consecutive response lines together
          if (!isPrompt) {
            // Find all consecutive response lines starting from this entry
            let responseLines: string[] = [];
            let _endIndex = index;
            for (let i = index; i < currentLoop.entries.length; i++) {
              if (currentLoop.entries[i].source !== "prompt") {
                responseLines.push(currentLoop.entries[i].line);
                _endIndex = i;
              } else {
                break;
              }
            }

            // Only render the grouped response at the first response line
            if (index > 0 && prevEntry?.source !== "prompt") {
              return null; // Skip - will be rendered as part of the group
            }

            const fullResponse = responseLines.join("\n");
            const lines = fullResponse.split("\n");
            const isExpanded = expandedResponses.has(entry.id);
            const shouldTruncate = lines.length > DEFAULT_MAX_LINES;
            const displayLines =
              shouldTruncate && !isExpanded
                ? lines.slice(0, DEFAULT_MAX_LINES).join("\n")
                : fullResponse;

            return (
              <div key={entry.id}>
                {isFirstResponse && (
                  <div className="flex items-center gap-2 text-emerald-400 text-xs font-semibold mb-2 mt-1">
                    <Bot className="w-4 h-4" />
                    <span>RESPONSE</span>
                    <span className="text-muted-foreground font-normal">{lines.length} lines</span>
                  </div>
                )}
                <div className="py-0.5 pl-6">
                  <span className="text-foreground whitespace-pre-wrap break-words">
                    {displayLines}
                  </span>
                  {shouldTruncate && (
                    <button
                      onClick={() => toggleResponseExpand(entry.id)}
                      className="mt-2 flex items-center gap-1 text-xs text-primary hover:text-primary/80 transition-colors"
                    >
                      {isExpanded ? (
                        <>
                          <ChevronUp className="w-3 h-3" />
                          Show less
                        </>
                      ) : (
                        <>
                          <ChevronDown className="w-3 h-3" />
                          Show {lines.length - DEFAULT_MAX_LINES} more lines
                        </>
                      )}
                    </button>
                  )}
                </div>
              </div>
            );
          }

          // Prompt styling
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
        })}
      </div>

      {/* Prompt input for continuing the conversation */}
      <div className="flex-shrink-0 mt-3 flex gap-2">
        <textarea
          ref={textareaRef}
          value={promptInput}
          onChange={(e) => setPromptInput(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder="Continue the conversation... (Enter to send, Shift+Enter for newline)"
          disabled={isSending || isAiWorking}
          className="flex-1 px-3 py-2 bg-background border border-border rounded-lg text-sm resize-none min-h-[40px] max-h-[120px] focus:outline-none focus:ring-2 focus:ring-primary disabled:opacity-50 disabled:cursor-not-allowed"
          rows={1}
        />
        <button
          onClick={handleSendPrompt}
          disabled={!promptInput.trim() || isSending || isAiWorking}
          className="px-4 py-2 bg-primary text-primary-foreground rounded-lg font-medium hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors flex items-center gap-2"
          title="Send prompt (Enter)"
        >
          {isSending ? <Loader2 className="w-4 h-4 animate-spin" /> : <Send className="w-4 h-4" />}
        </button>
      </div>
    </div>
  );
}
