/**
 * SessionSubTab.tsx
 *
 * Unified AI Run view that shows:
 * - Current AI status (Running/Idle/Resumable) at the top
 * - Run history selector to filter by automation run
 * - Continue button prominently when a run is resumable
 * - Split panel: Quick actions on left, Live AI Output on right
 * - Stop button when AI is running
 *
 * This is the primary view users should see when AI is active.
 */

import { invoke } from "@tauri-apps/api/core";
import { useState, useEffect, useCallback, useMemo } from "react";
import {
  Play,
  Square,
  Loader2,
  RotateCcw,
  ToggleLeft,
  ToggleRight,
  Sparkles,
  Clock,
  CheckCircle,
  XCircle,
  AlertTriangle,
  ChevronDown,
  ChevronUp,
  Brain,
  Activity,
  History,
  Filter,
  Trash2,
  Settings,
  Calendar,
  CheckSquare,
  X,
  Wrench,
} from "lucide-react";
import { AiOutputTab } from "../AiOutputTab";
import type { AiOutputLine } from "../AiOutputTab";
import { issueTracker } from "../../services";
import type { DetectedIssue } from "../../types/issues";
import { groupEntriesIntoLoops, type AiLoop } from "../../types/aiLoop";
import { useAutoContinue } from "../../contexts";

interface SessionSubTabProps {
  aiOutputLines: AiOutputLine[];
  onClearAiOutput: () => void;
  onAddAiOutputLine?: (line: Omit<AiOutputLine, "id">) => void;
  onNavigateToLibrary: () => void;
}

interface ResumableTask {
  name: string;
  sessionsCount: number;
  maxSessions: number | null; // null = unlimited
  status: string;
}

/** Information about a single active session from the backend */
interface ActiveSessionInfo {
  id: string;
  name: string;
  status: string;
  started_at: string;
  uses_gui: boolean;
}

interface ActiveTaskInfo {
  name: string;
  type: "task" | "one_shot" | "builder";
  startedAt?: string;
  sessionsCount?: number;
  maxSessions?: number | null;
}

/** Represents a workflow session that groups multiple AI loops */
interface WorkflowSession {
  id: string;
  name: string;
  startTime: number;
  endTime: number;
  loopCount: number;
  isActive: boolean;
}

/** Information about a resumable workflow/task that can be continued */
interface _ResumableWorkflow {
  name: string;
  currentPhase: number;
  totalPhases: number;
  status: string;
}

/** Information about the currently active workflow */
interface _ActiveWorkflowInfo {
  name: string;
  type: string;
  iteration: number;
  maxIterations: number;
}

export function SessionSubTab({
  aiOutputLines,
  onClearAiOutput,
  onAddAiOutputLine,
  onNavigateToLibrary,
}: SessionSubTabProps) {
  // AI running state
  const [isRunning, setIsRunning] = useState(false);
  const [isStopping, setIsStopping] = useState(false);

  // Resumable task state
  const [resumableTask, setResumableTask] = useState<ResumableTask | null>(null);
  const [isResuming, setIsResuming] = useState(false);
  const [isForceContinuing, setIsForceContinuing] = useState(false);

  // Auto-continue settings (from context - syncs across all components)
  const {
    enabled: autoContinueEnabled,
    loading: autoContinueLoading,
    toggle: toggleAutoContinue,
  } = useAutoContinue();

  // Auto-fix on failure state
  const [autoFixEnabled, setAutoFixEnabled] = useState(false);
  const [autoFixLoading, setAutoFixLoading] = useState(false);

  // Active task info (when running)
  const [activeTask, setActiveTask] = useState<ActiveTaskInfo | null>(null);

  // All currently active sessions (for concurrent session display)
  const [activeSessions, setActiveSessions] = useState<ActiveSessionInfo[]>([]);

  // Session/workflow filter
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);
  const [showSessionSelector, setShowSessionSelector] = useState(false);

  // Run management (for deletion)
  const [isManagingRuns, setIsManagingRuns] = useState(false);
  const [selectedForDeletion, setSelectedForDeletion] = useState<Set<string>>(new Set());
  const [deleteBeforeDate, setDeleteBeforeDate] = useState<string>("");
  const [isDeleting, setIsDeleting] = useState(false);

  // Issues tracking
  const [sessionIssues, setSessionIssues] = useState<DetectedIssue[]>([]);
  const [showIssues, setShowIssues] = useState(true);

  // Last result message
  const [lastResult, setLastResult] = useState<{ success: boolean; message: string } | null>(null);

  // Per-run auto-continue setting (distinct from global auto-continue)
  const [runAutoContinue, setRunAutoContinue] = useState<boolean | null>(null);
  const [runAutoContinueLoading, setRunAutoContinueLoading] = useState(false);

  // Group AI output lines into loops
  const loops = useMemo(() => groupEntriesIntoLoops(aiOutputLines), [aiOutputLines]);

  // Extract workflow sessions from loops
  // Sessions are grouped ONLY by explicit sessionId from the backend
  // No time-based grouping - each session is identified by its unique sessionId
  const workflowSessions = useMemo(() => {
    const sessions: WorkflowSession[] = [];

    // Helper to find the best name from a loop's entries
    const findBestLoopName = (loop: AiLoop): string | null => {
      // First check if loop has an explicit session name
      if (loop.sessionName && loop.sessionName !== "AI Response") {
        return loop.sessionName;
      }
      // Then check the prompt preview if it's meaningful
      if (loop.promptPreview && loop.promptPreview !== "AI Response") {
        return loop.promptPreview;
      }
      // Look through entries for the first actual prompt
      for (const entry of loop.entries) {
        if (entry.source === "prompt" && entry.line.trim()) {
          const cleaned = entry.line.replace(/\n/g, " ").trim();
          if (cleaned.length > 0) {
            return cleaned.length > 50 ? cleaned.substring(0, 47) + "..." : cleaned;
          }
        }
      }
      return null;
    };

    // Group loops by sessionId
    interface TempSession {
      id: string;
      loops: AiLoop[];
      startTime: number;
      endTime: number;
    }
    const sessionMap = new Map<string, TempSession>();

    for (const loop of loops) {
      // Use explicit sessionId, or fall back to loop id for legacy data without sessionId
      const sessionId = loop.sessionId || loop.id;

      if (sessionMap.has(sessionId)) {
        // Add loop to existing session
        const session = sessionMap.get(sessionId)!;
        session.loops.push(loop);
        session.endTime = Math.max(session.endTime, loop.endTime);
        session.startTime = Math.min(session.startTime, loop.startTime);
      } else {
        // Start new session
        sessionMap.set(sessionId, {
          id: sessionId,
          loops: [loop],
          startTime: loop.startTime,
          endTime: loop.endTime,
        });
      }
    }

    // Convert map to array and find best names
    for (const temp of sessionMap.values()) {
      let bestName: string | null = null;

      // Priority 1: Look for explicit sessionName from any loop (this is the task title)
      for (const loop of temp.loops) {
        if (loop.sessionName && loop.sessionName !== "AI Response") {
          bestName = loop.sessionName;
          break;
        }
      }

      // Priority 2: Look for sessionName in any entry (handles cases where loop doesn't have it but entry does)
      if (!bestName) {
        for (const loop of temp.loops) {
          for (const entry of loop.entries) {
            if (entry.sessionName && entry.sessionName !== "AI Response") {
              bestName = entry.sessionName;
              break;
            }
          }
          if (bestName) break;
        }
      }

      // Priority 3: Fall back to first meaningful prompt or other name
      if (!bestName) {
        for (const loop of temp.loops) {
          const loopName = findBestLoopName(loop);
          if (loopName) {
            bestName = loopName;
            break;
          }
        }
      }

      // Priority 4: Generic name with timestamp (only as last resort)
      if (!bestName) {
        bestName = "Untitled Run";
      }

      sessions.push({
        id: temp.id,
        name: bestName,
        startTime: temp.startTime,
        endTime: temp.endTime,
        loopCount: temp.loops.length,
        // Session is active if it's in the active sessions list from the backend
        isActive: activeSessions.some((s) => s.id === temp.id),
      });
    }

    // Sort by most recent first
    return sessions.sort((a, b) => b.endTime - a.endTime);
  }, [loops, activeSessions]);

  // Get the selected session or default to the most recent
  const currentSession = useMemo(() => {
    if (!selectedSessionId && workflowSessions.length > 0) {
      return workflowSessions[0]; // Most recent
    }
    return workflowSessions.find((s) => s.id === selectedSessionId) || null;
  }, [selectedSessionId, workflowSessions]);

  // Filter loops by selected session
  const filteredLoops = useMemo(() => {
    if (!currentSession) return loops;

    // Find loops that belong to this session's time range (with some buffer)
    const BUFFER_MS = 1000; // 1 second buffer
    return loops.filter(
      (loop) =>
        loop.startTime >= currentSession.startTime - BUFFER_MS &&
        loop.endTime <= currentSession.endTime + BUFFER_MS,
    );
  }, [loops, currentSession]);

  // Get filtered AI output lines based on selected session
  const filteredAiOutputLines = useMemo(() => {
    if (!currentSession) return aiOutputLines;

    // Get all entry IDs from filtered loops
    const filteredEntryIds = new Set<string>();
    for (const loop of filteredLoops) {
      for (const entry of loop.entries) {
        filteredEntryIds.add(entry.id);
      }
    }

    return aiOutputLines.filter((line) => filteredEntryIds.has(line.id));
  }, [aiOutputLines, currentSession, filteredLoops]);

  // Subscribe to issue tracker updates
  useEffect(() => {
    const updateIssues = () => {
      setSessionIssues(issueTracker.getSessionIssues());
    };
    updateIssues();
    const unsubscribe = issueTracker.subscribe(updateIssues);
    return unsubscribe;
  }, []);

  // Load auto-fix setting on mount
  useEffect(() => {
    const loadAutoFixSetting = async () => {
      try {
        const response = await fetch("http://localhost:9876/session/auto-fix");
        const result = await response.json();
        if (result.success) {
          setAutoFixEnabled(result.data?.enabled ?? false);
        }
      } catch (error) {
        console.error("Failed to load auto-fix setting:", error);
      }
    };
    loadAutoFixSetting();
  }, []);

  // Toggle auto-fix setting
  const toggleAutoFix = useCallback(async () => {
    if (autoFixLoading) return;

    setAutoFixLoading(true);
    try {
      const response = await fetch("http://localhost:9876/session/auto-fix", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ enabled: !autoFixEnabled }),
      });
      const result = await response.json();
      if (result.success) {
        setAutoFixEnabled(result.data?.enabled ?? !autoFixEnabled);
      }
    } catch (error) {
      console.error("Failed to toggle auto-fix setting:", error);
    } finally {
      setAutoFixLoading(false);
    }
  }, [autoFixEnabled, autoFixLoading]);

  // Load per-run auto-continue setting when a specific run is selected
  useEffect(() => {
    // Only fetch when a specific session is selected (not "All Runs")
    if (!selectedSessionId) {
      setRunAutoContinue(null);
      return;
    }

    const loadRunAutoContinue = async () => {
      try {
        const response = await fetch(
          `http://localhost:9876/task-runs/${selectedSessionId}/auto-continue`,
        );
        const result = await response.json();
        if (result.auto_continue !== undefined) {
          setRunAutoContinue(result.auto_continue);
        }
      } catch (error) {
        console.error("Failed to load per-run auto-continue setting:", error);
        setRunAutoContinue(null);
      }
    };
    loadRunAutoContinue();
  }, [selectedSessionId]);

  // Toggle per-run auto-continue setting
  const toggleRunAutoContinue = useCallback(async () => {
    if (!selectedSessionId || runAutoContinueLoading || runAutoContinue === null) return;

    setRunAutoContinueLoading(true);
    try {
      const response = await fetch(
        `http://localhost:9876/task-runs/${selectedSessionId}/auto-continue`,
        {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ auto_continue: !runAutoContinue }),
        },
      );
      const result = await response.json();
      if (result.success) {
        setRunAutoContinue(result.auto_continue);
      }
    } catch (error) {
      console.error("Failed to toggle per-run auto-continue setting:", error);
    } finally {
      setRunAutoContinueLoading(false);
    }
  }, [selectedSessionId, runAutoContinue, runAutoContinueLoading]);

  // Check for resumable tasks and running state periodically
  useEffect(() => {
    const checkStatus = async () => {
      try {
        const response = await fetch("http://localhost:9876/workflow/resumable");
        const result = await response.json();
        if (result.success) {
          // Update isRunning from the API
          if (result.data?.is_running !== undefined) {
            setIsRunning(result.data.is_running);
          }
          // Note: auto_continue_enabled is now managed by AutoContinueContext
          // Only show Continue if there's a resumable task AND nothing is running
          if (result.data?.has_resumable && !result.data?.is_running) {
            setResumableTask({
              name: result.data.name || "Unknown Task",
              sessionsCount: result.data.sessions_count || 0,
              maxSessions: result.data.max_sessions || null,
              status: result.data.status || "unknown",
            });
          } else {
            setResumableTask(null);
          }

          // Set active task info when running
          if (result.data?.is_running && result.data?.name) {
            setActiveTask({
              name: result.data.name,
              type: result.data.task_type || "task",
              sessionsCount: result.data.sessions_count,
              maxSessions: result.data.max_sessions,
            });
          } else if (!result.data?.is_running) {
            setActiveTask(null);
          }

          // Update all active sessions (for concurrent session display)
          if (result.data?.active_sessions) {
            setActiveSessions(result.data.active_sessions);
          } else {
            setActiveSessions([]);
          }
        }
      } catch (error) {
        console.error("Failed to check workflow status:", error);
      }
    };

    // Check immediately
    checkStatus();

    // Poll every 2 seconds for responsive updates
    const interval = setInterval(checkStatus, 2000);

    return () => clearInterval(interval);
  }, []);

  // Handler to resume a workflow
  const handleResumeWorkflow = useCallback(async () => {
    if (!resumableTask || isResuming) return;

    setIsResuming(true);
    try {
      const response = await fetch("http://localhost:9876/workflow/resume", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
      });
      const result = await response.json();

      if (result.success) {
        setResumableTask(null);
        setIsRunning(true);
        setLastResult({ success: true, message: `Resuming task: ${resumableTask.name}` });
      } else {
        setLastResult({
          success: false,
          message: result.error || "Failed to resume workflow",
        });
      }
    } catch (error) {
      console.error("Failed to resume workflow:", error);
      setLastResult({
        success: false,
        message: `Failed to resume: ${error instanceof Error ? error.message : String(error)}`,
      });
    } finally {
      setIsResuming(false);
    }
  }, [resumableTask, isResuming]);

  // Handler to force continue a stopped session
  // If a specific session is selected, continue that one; otherwise continue most recent
  const handleForceContinue = useCallback(async () => {
    if (isForceContinuing) return;

    setIsForceContinuing(true);
    try {
      // If a specific session is selected, pass its ID to continue that specific run
      const requestBody: { task_run_id?: string } = {};
      if (currentSession) {
        requestBody.task_run_id = currentSession.id;
      }

      const response = await fetch("http://localhost:9876/workflow/force-continue", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(requestBody),
      });
      const result = await response.json();

      if (result.success) {
        setIsRunning(true);
        const sessionInfo = currentSession ? ` "${currentSession.name}"` : "";
        setLastResult({
          success: true,
          message: `Continuing${sessionInfo}. AI will resume with context from last output.`,
        });
      } else {
        setLastResult({
          success: false,
          message: result.error || "Failed to continue",
        });
      }
    } catch (error) {
      console.error("Failed to force continue:", error);
      setLastResult({
        success: false,
        message: `Failed to continue: ${error instanceof Error ? error.message : String(error)}`,
      });
    } finally {
      setIsForceContinuing(false);
    }
  }, [isForceContinuing, currentSession]);

  // Handler to stop AI
  const handleStopAi = useCallback(async () => {
    setIsStopping(true);
    try {
      const response = await fetch("http://localhost:9876/stop-ai-analysis", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
      });
      const result = await response.json();
      if (result.success) {
        setLastResult({ success: true, message: "Stop requested. AI will exit gracefully." });
      } else {
        setLastResult({ success: false, message: result.error || "Failed to stop AI" });
      }
    } catch (error) {
      console.error("Failed to stop AI:", error);
      setLastResult({
        success: false,
        message: `Failed to stop: ${error instanceof Error ? error.message : String(error)}`,
      });
    } finally {
      setIsStopping(false);
    }
  }, []);

  // Handle session selection
  const handleSelectSession = useCallback((sessionId: string | null) => {
    setSelectedSessionId(sessionId);
    setShowSessionSelector(false);
  }, []);

  // Toggle run management mode
  const handleToggleManagement = useCallback(() => {
    setIsManagingRuns((prev) => {
      if (prev) {
        // Exiting management mode - clear selections
        setSelectedForDeletion(new Set());
        setDeleteBeforeDate("");
      }
      return !prev;
    });
  }, []);

  // Toggle session selection for deletion
  const handleToggleSessionSelection = useCallback((sessionId: string) => {
    setSelectedForDeletion((prev) => {
      const next = new Set(prev);
      if (next.has(sessionId)) {
        next.delete(sessionId);
      } else {
        next.add(sessionId);
      }
      return next;
    });
  }, []);

  // Select all sessions before a date
  const handleSelectBeforeDate = useCallback(
    (dateStr: string) => {
      setDeleteBeforeDate(dateStr);
      if (!dateStr) {
        return;
      }
      const cutoffDate = new Date(dateStr);
      cutoffDate.setHours(23, 59, 59, 999); // End of the selected day
      const cutoffTimestamp = cutoffDate.getTime();

      const toSelect = new Set<string>();
      for (const session of workflowSessions) {
        if (session.endTime <= cutoffTimestamp) {
          toSelect.add(session.id);
        }
      }
      setSelectedForDeletion(toSelect);
    },
    [workflowSessions],
  );

  // Delete selected runs (sessions)
  const handleDeleteRuns = useCallback(async () => {
    if (selectedForDeletion.size === 0) return;

    setIsDeleting(true);

    try {
      // Get the session IDs to delete
      const sessionIds = Array.from(selectedForDeletion);

      // Delete checkpoint files for these sessions via Tauri command
      const checkpointResult = await invoke<{
        success: boolean;
        message?: string;
        data?: { deleted_count: number };
      }>("delete_session_checkpoints", { sessionIds });

      if (!checkpointResult.success) {
        console.warn("Failed to delete some checkpoints:", checkpointResult.message);
      } else {
        console.log(`Deleted ${checkpointResult.data?.deleted_count ?? 0} checkpoint files`);
      }

      // If deleting all sessions, clear the AI output log entirely
      if (selectedForDeletion.size === workflowSessions.length) {
        // Clear the AI output log file via Tauri command
        await invoke("clear_ai_output_log");
        // Clear in-memory state
        onClearAiOutput();
      } else {
        // For partial deletion, we still need to clear all (selective deletion not yet supported)
        // The checkpoint files are deleted, so runs won't reappear after app restart
        await invoke("clear_ai_output_log");
        onClearAiOutput();
      }
    } catch (error) {
      console.error("Failed to delete runs:", error);
    } finally {
      // Reset state
      setSelectedForDeletion(new Set());
      setDeleteBeforeDate("");
      setIsManagingRuns(false);
      setIsDeleting(false);
    }
  }, [selectedForDeletion, workflowSessions, onClearAiOutput]);

  // Select/deselect all sessions
  const handleSelectAll = useCallback(() => {
    if (selectedForDeletion.size === workflowSessions.length) {
      // Deselect all
      setSelectedForDeletion(new Set());
    } else {
      // Select all
      setSelectedForDeletion(new Set(workflowSessions.map((s) => s.id)));
    }
  }, [workflowSessions, selectedForDeletion.size]);

  // Get issue summary
  const issueSummary = useMemo(() => {
    const summary = {
      total: sessionIssues.length,
      detected: 0,
      inProgress: 0,
      resolved: 0,
      critical: 0,
    };
    for (const issue of sessionIssues) {
      if (issue.status === "detected") summary.detected++;
      if (issue.status === "in_progress") summary.inProgress++;
      if (issue.status === "resolved") summary.resolved++;
      if (issue.severity === "critical") summary.critical++;
    }
    return summary;
  }, [sessionIssues]);

  // Determine the status for the banner
  const statusInfo = useMemo(() => {
    if (isRunning) {
      // Show count of active sessions if multiple
      const sessionCount = activeSessions.length;
      const sessionLabel = activeTask?.sessionsCount
        ? `Session ${activeTask.sessionsCount}${activeTask.maxSessions ? ` of ${activeTask.maxSessions}` : ""}`
        : "Processing...";
      const subtitle = sessionCount > 1 ? `${sessionCount} runs in progress` : sessionLabel;

      return {
        status: "running" as const,
        color: "emerald",
        icon: Loader2,
        title:
          sessionCount > 1 ? `${sessionCount} Runs in Progress` : activeTask?.name || "AI Running",
        subtitle,
        sessionCount,
      };
    }
    if (resumableTask) {
      const sessionLabel = resumableTask.maxSessions
        ? `Session ${resumableTask.sessionsCount} of ${resumableTask.maxSessions}`
        : `Session ${resumableTask.sessionsCount}`;
      return {
        status: "resumable" as const,
        color: "orange",
        icon: Clock,
        title: resumableTask.name,
        subtitle: `Paused at ${sessionLabel}`,
      };
    }
    return {
      status: "idle" as const,
      color: "gray",
      icon: Sparkles,
      title: "No Active Run",
      subtitle: "Start a task from the Library",
    };
  }, [isRunning, resumableTask, activeTask, activeSessions]);

  // Format time for display
  const formatTime = (timestamp: number) => {
    const date = new Date(timestamp);
    return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  };

  const formatDate = (timestamp: number) => {
    const date = new Date(timestamp);
    return date.toLocaleDateString([], { month: "short", day: "numeric" });
  };

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Status Banner */}
      <div
        className={`flex-shrink-0 p-4 border-b ${
          statusInfo.status === "running"
            ? "bg-emerald-500/10 border-emerald-500/30"
            : statusInfo.status === "resumable"
              ? "bg-orange-500/10 border-orange-500/30"
              : "bg-muted/30 border-border"
        }`}
      >
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            {statusInfo.status === "running" ? (
              <div className="w-10 h-10 rounded-full bg-emerald-500/20 flex items-center justify-center">
                <Loader2 className="w-5 h-5 text-emerald-500 animate-spin" />
              </div>
            ) : statusInfo.status === "resumable" ? (
              <div className="w-10 h-10 rounded-full bg-orange-500/20 flex items-center justify-center">
                <Clock className="w-5 h-5 text-orange-500" />
              </div>
            ) : (
              <div className="w-10 h-10 rounded-full bg-muted flex items-center justify-center">
                <Sparkles className="w-5 h-5 text-muted-foreground" />
              </div>
            )}
            <div>
              <h3 className="font-semibold text-foreground">{statusInfo.title}</h3>
              <p className="text-sm text-muted-foreground">{statusInfo.subtitle}</p>
            </div>
          </div>

          {/* Action Buttons */}
          <div className="flex items-center gap-3">
            {/* Active Sessions List - Show when multiple sessions */}
            {activeSessions.length > 1 && (
              <div className="flex items-center gap-2 text-xs">
                {activeSessions.map((session) => (
                  <span
                    key={session.id}
                    className={`px-2 py-1 rounded-full ${
                      session.uses_gui
                        ? "bg-purple-500/20 text-purple-400"
                        : "bg-blue-500/20 text-blue-400"
                    }`}
                    title={session.uses_gui ? "GUI Session" : "Non-GUI Session"}
                  >
                    {session.name.length > 20
                      ? session.name.substring(0, 17) + "..."
                      : session.name}
                  </span>
                ))}
              </div>
            )}

            {/* Continue Button - Prominent when resumable */}
            {resumableTask && !isRunning && (
              <button
                onClick={handleResumeWorkflow}
                disabled={isResuming}
                className="flex items-center gap-2 px-4 py-2 bg-orange-500 text-white rounded-lg font-medium hover:bg-orange-600 disabled:opacity-50 transition-colors"
              >
                {isResuming ? (
                  <>
                    <Loader2 className="w-4 h-4 animate-spin" />
                    Resuming...
                  </>
                ) : (
                  <>
                    <Play className="w-4 h-4" />
                    Continue Task
                  </>
                )}
              </button>
            )}

            {/* Continue Button - When stopped unexpectedly (has output but not running/resumable) */}
            {!isRunning && !resumableTask && aiOutputLines.length > 0 && (
              <button
                onClick={handleForceContinue}
                disabled={isForceContinuing}
                className="flex items-center gap-2 px-4 py-2 bg-amber-600 text-white rounded-lg font-medium hover:bg-amber-700 disabled:opacity-50 transition-colors"
                title="Continue a run that stopped. Uses recent output as context."
              >
                {isForceContinuing ? (
                  <>
                    <Loader2 className="w-4 h-4 animate-spin" />
                    Starting...
                  </>
                ) : (
                  <>
                    <RotateCcw className="w-4 h-4" />
                    Continue
                  </>
                )}
              </button>
            )}

            {/* Stop Button - When running */}
            {isRunning && (
              <button
                onClick={handleStopAi}
                disabled={isStopping}
                className="flex items-center gap-2 px-4 py-2 bg-red-500 text-white rounded-lg font-medium hover:bg-red-600 disabled:opacity-50 transition-colors"
              >
                {isStopping ? (
                  <>
                    <Loader2 className="w-4 h-4 animate-spin" />
                    Stopping...
                  </>
                ) : (
                  <>
                    <Square className="w-4 h-4" />
                    Stop AI
                  </>
                )}
              </button>
            )}

            {/* Go to Library - When idle */}
            {statusInfo.status === "idle" && (
              <button
                onClick={onNavigateToLibrary}
                className="flex items-center gap-2 px-4 py-2 bg-primary text-primary-foreground rounded-lg font-medium hover:bg-primary/90 transition-colors"
              >
                <Sparkles className="w-4 h-4" />
                Start from Library
              </button>
            )}
          </div>
        </div>

        {/* Last Result Message */}
        {lastResult && (
          <div
            className={`mt-3 flex items-center gap-2 text-sm ${
              lastResult.success ? "text-emerald-400" : "text-red-400"
            }`}
          >
            {lastResult.success ? (
              <CheckCircle className="w-4 h-4" />
            ) : (
              <XCircle className="w-4 h-4" />
            )}
            {lastResult.message}
          </div>
        )}
      </div>

      {/* Main Content - Split Panel */}
      <div className="flex-1 flex min-h-0 overflow-hidden">
        {/* Left Panel - Settings & Issues */}
        <div className="w-80 flex-shrink-0 border-r border-border overflow-y-auto p-4 space-y-4">
          {/* Run Selector */}
          {workflowSessions.length > 0 && (
            <div className="card">
              {/* Header */}
              <div className="flex items-center justify-between p-3 border-b border-border">
                <button
                  onClick={() => setShowSessionSelector(!showSessionSelector)}
                  className="flex items-center gap-2 flex-1 hover:bg-muted/50 rounded p-1 -m-1 transition-colors"
                >
                  <History className="w-4 h-4 text-primary" />
                  <div className="text-left">
                    <span className="text-sm font-medium block">
                      {isManagingRuns
                        ? "Manage Runs"
                        : currentSession
                          ? currentSession.name.length > 15
                            ? currentSession.name.substring(0, 15) + "..."
                            : currentSession.name
                          : "All Runs"}
                    </span>
                    <span className="text-xs text-muted-foreground">
                      {isManagingRuns
                        ? `${selectedForDeletion.size} selected`
                        : currentSession
                          ? `${currentSession.loopCount} conversation${currentSession.loopCount !== 1 ? "s" : ""}`
                          : `${workflowSessions.length} run${workflowSessions.length !== 1 ? "s" : ""}`}
                    </span>
                  </div>
                </button>
                <div className="flex items-center gap-1">
                  {!isManagingRuns ? (
                    <button
                      onClick={handleToggleManagement}
                      className="p-1.5 text-muted-foreground hover:text-foreground hover:bg-muted/50 rounded transition-colors"
                      title="Manage runs"
                    >
                      <Settings className="w-4 h-4" />
                    </button>
                  ) : (
                    <button
                      onClick={handleToggleManagement}
                      className="p-1.5 text-muted-foreground hover:text-foreground hover:bg-muted/50 rounded transition-colors"
                      title="Cancel"
                    >
                      <X className="w-4 h-4" />
                    </button>
                  )}
                  <button
                    onClick={() => setShowSessionSelector(!showSessionSelector)}
                    className="p-1 text-muted-foreground hover:text-foreground hover:bg-muted/50 rounded transition-colors"
                    title={showSessionSelector ? "Collapse" : "Expand"}
                  >
                    {showSessionSelector ? (
                      <ChevronUp className="w-4 h-4" />
                    ) : (
                      <ChevronDown className="w-4 h-4" />
                    )}
                  </button>
                </div>
              </div>

              {/* Management Controls */}
              {isManagingRuns && showSessionSelector && (
                <div className="px-3 py-2 border-b border-border bg-muted/30 space-y-2">
                  {/* Date filter */}
                  <div className="flex items-center gap-2">
                    <Calendar className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                    <input
                      type="date"
                      value={deleteBeforeDate}
                      onChange={(e) => handleSelectBeforeDate(e.target.value)}
                      className="flex-1 text-xs bg-background border border-border rounded px-2 py-1"
                      title="Select runs before this date"
                    />
                  </div>

                  {/* Select all / Delete buttons */}
                  <div className="flex items-center gap-2">
                    <button
                      onClick={handleSelectAll}
                      className="flex items-center gap-1 px-2 py-1 text-xs bg-muted hover:bg-muted/80 rounded transition-colors"
                    >
                      <CheckSquare className="w-3 h-3" />
                      {selectedForDeletion.size === workflowSessions.length
                        ? "Deselect All"
                        : "Select All"}
                    </button>
                    <button
                      onClick={handleDeleteRuns}
                      disabled={selectedForDeletion.size === 0 || isDeleting}
                      className="flex items-center gap-1 px-2 py-1 text-xs bg-red-500/20 text-red-400 hover:bg-red-500/30 rounded transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                    >
                      {isDeleting ? (
                        <Loader2 className="w-3 h-3 animate-spin" />
                      ) : (
                        <Trash2 className="w-3 h-3" />
                      )}
                      Delete ({selectedForDeletion.size})
                    </button>
                  </div>
                </div>
              )}

              {/* Run List */}
              {showSessionSelector && (
                <div className="px-2 pb-2 pt-1 space-y-1 max-h-48 overflow-y-auto">
                  {/* All Runs option (only in view mode) */}
                  {!isManagingRuns && (
                    <button
                      onClick={() => handleSelectSession(null)}
                      className={`w-full flex items-center gap-2 p-2 rounded text-left text-sm hover:bg-muted/50 transition-colors ${
                        !selectedSessionId ? "bg-muted" : ""
                      }`}
                    >
                      <Filter className="w-3 h-3 text-muted-foreground" />
                      <span>All Runs</span>
                      <span className="text-xs text-muted-foreground ml-auto">
                        {workflowSessions.length} runs
                      </span>
                    </button>
                  )}

                  {/* Individual runs */}
                  {workflowSessions.map((session) => (
                    <div
                      key={session.id}
                      className={`flex items-start gap-2 p-2 rounded text-sm hover:bg-muted/50 transition-colors ${
                        isManagingRuns
                          ? selectedForDeletion.has(session.id)
                            ? "bg-red-500/10"
                            : ""
                          : selectedSessionId === session.id
                            ? "bg-muted"
                            : ""
                      }`}
                    >
                      {isManagingRuns ? (
                        <input
                          type="checkbox"
                          checked={selectedForDeletion.has(session.id)}
                          onChange={() => handleToggleSessionSelection(session.id)}
                          className="mt-0.5 flex-shrink-0 accent-red-500"
                        />
                      ) : (
                        <button
                          onClick={() => handleSelectSession(session.id)}
                          className="flex-1 flex items-start gap-2 text-left"
                        >
                          <Brain className="w-3 h-3 text-primary mt-0.5 flex-shrink-0" />
                          <div className="flex-1 min-w-0">
                            <div className="flex items-center gap-1">
                              <span className="font-medium truncate">
                                {session.name.length > 18
                                  ? session.name.substring(0, 18) + "..."
                                  : session.name}
                              </span>
                              {session.isActive && (
                                <span className="text-[10px] px-1 py-0.5 bg-emerald-500/20 text-emerald-400 rounded">
                                  Active
                                </span>
                              )}
                            </div>
                            <div className="text-xs text-muted-foreground">
                              {formatDate(session.startTime)} {formatTime(session.startTime)} -{" "}
                              {session.loopCount} conv{session.loopCount !== 1 ? "s" : ""}
                            </div>
                          </div>
                        </button>
                      )}
                      {isManagingRuns && (
                        <button
                          onClick={() => handleToggleSessionSelection(session.id)}
                          className="flex-1 flex items-start gap-2 text-left"
                        >
                          <div className="flex-1 min-w-0">
                            <div className="flex items-center gap-1">
                              <span className="font-medium truncate">
                                {session.name.length > 16
                                  ? session.name.substring(0, 16) + "..."
                                  : session.name}
                              </span>
                              {session.isActive && (
                                <span className="text-[10px] px-1 py-0.5 bg-emerald-500/20 text-emerald-400 rounded">
                                  Active
                                </span>
                              )}
                            </div>
                            <div className="text-xs text-muted-foreground">
                              {formatDate(session.startTime)} - {session.loopCount} conv
                              {session.loopCount !== 1 ? "s" : ""}
                            </div>
                          </div>
                        </button>
                      )}
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}

          {/* Auto-Continue Toggle (Global) */}
          <div className="card p-3">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <RotateCcw className="w-4 h-4 text-orange-500" />
                <div>
                  <span className="text-sm font-medium">Auto-Continue</span>
                  <p className="text-xs text-muted-foreground">
                    {autoContinueEnabled ? "Resume on restart" : "Manual resume"}
                  </p>
                </div>
              </div>
              <button
                onClick={toggleAutoContinue}
                disabled={autoContinueLoading}
                className={`flex items-center transition-colors ${autoContinueEnabled ? "text-orange-500" : "text-muted-foreground"} ${autoContinueLoading ? "opacity-50" : ""}`}
                title={autoContinueEnabled ? "Auto-continue enabled" : "Auto-continue disabled"}
              >
                {autoContinueLoading ? (
                  <Loader2 className="w-6 h-6 animate-spin" />
                ) : autoContinueEnabled ? (
                  <ToggleRight className="w-6 h-6" />
                ) : (
                  <ToggleLeft className="w-6 h-6" />
                )}
              </button>
            </div>
          </div>

          {/* Per-Run Auto-Continue Toggle - Only shown when a specific run is selected */}
          {selectedSessionId && runAutoContinue !== null && (
            <div className="card p-3 border-l-2 border-l-purple-500/50">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <Play className="w-4 h-4 text-purple-500" />
                  <div>
                    <span className="text-sm font-medium">Run Auto-Continue</span>
                    <p className="text-xs text-muted-foreground">
                      {runAutoContinue
                        ? "This run will auto-resume"
                        : "This run requires manual resume"}
                    </p>
                  </div>
                </div>
                <button
                  onClick={toggleRunAutoContinue}
                  disabled={runAutoContinueLoading}
                  className={`flex items-center transition-colors ${runAutoContinue ? "text-purple-500" : "text-muted-foreground"} ${runAutoContinueLoading ? "opacity-50" : ""}`}
                  title={
                    runAutoContinue
                      ? "Per-run auto-continue enabled"
                      : "Per-run auto-continue disabled"
                  }
                >
                  {runAutoContinueLoading ? (
                    <Loader2 className="w-6 h-6 animate-spin" />
                  ) : runAutoContinue ? (
                    <ToggleRight className="w-6 h-6" />
                  ) : (
                    <ToggleLeft className="w-6 h-6" />
                  )}
                </button>
              </div>
            </div>
          )}

          {/* Auto-Fix on Failure Toggle */}
          <div className="card p-3">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <Wrench className="w-4 h-4 text-blue-500" />
                <div>
                  <span className="text-sm font-medium">Auto-Fix</span>
                  <p className="text-xs text-muted-foreground">
                    {autoFixEnabled ? "Fix issues on failure" : "Manual fix"}
                  </p>
                </div>
              </div>
              <button
                onClick={toggleAutoFix}
                disabled={autoFixLoading}
                className={`flex items-center transition-colors ${autoFixEnabled ? "text-blue-500" : "text-muted-foreground"} ${autoFixLoading ? "opacity-50" : ""}`}
                title={autoFixEnabled ? "Auto-fix enabled" : "Auto-fix disabled"}
              >
                {autoFixLoading ? (
                  <Loader2 className="w-6 h-6 animate-spin" />
                ) : autoFixEnabled ? (
                  <ToggleRight className="w-6 h-6" />
                ) : (
                  <ToggleLeft className="w-6 h-6" />
                )}
              </button>
            </div>
          </div>

          {/* Issues Summary */}
          <div className="card">
            <button
              onClick={() => setShowIssues(!showIssues)}
              className="w-full flex items-center justify-between p-3 hover:bg-muted/50 transition-colors rounded-t-lg"
            >
              <div className="flex items-center gap-2">
                {issueSummary.total > 0 ? (
                  <AlertTriangle
                    className={`w-4 h-4 ${issueSummary.critical > 0 ? "text-red-500" : "text-orange-500"}`}
                  />
                ) : (
                  <CheckCircle className="w-4 h-4 text-green-500" />
                )}
                <span className="text-sm font-medium">
                  {issueSummary.total > 0 ? `${issueSummary.total} Issues` : "No Issues"}
                </span>
              </div>
              <div className="flex items-center gap-2">
                {issueSummary.detected > 0 && (
                  <span className="text-xs px-1.5 py-0.5 bg-red-500/20 text-red-400 rounded">
                    {issueSummary.detected} open
                  </span>
                )}
                {showIssues ? (
                  <ChevronUp className="w-4 h-4" />
                ) : (
                  <ChevronDown className="w-4 h-4" />
                )}
              </div>
            </button>

            {showIssues && issueSummary.total > 0 && (
              <div className="px-3 pb-3 space-y-2 max-h-48 overflow-y-auto border-t border-border">
                {sessionIssues.slice(0, 5).map((issue) => (
                  <div
                    key={issue.id}
                    className="flex items-start gap-2 text-xs py-2 border-b border-border/50 last:border-b-0"
                  >
                    {issue.status === "resolved" ? (
                      <CheckCircle className="w-3 h-3 text-green-500 flex-shrink-0 mt-0.5" />
                    ) : (
                      <AlertTriangle
                        className={`w-3 h-3 flex-shrink-0 mt-0.5 ${
                          issue.severity === "critical" ? "text-red-500" : "text-orange-500"
                        }`}
                      />
                    )}
                    <div className="flex-1 min-w-0">
                      <p className="font-medium truncate">{issue.title}</p>
                      {issue.file && <p className="text-muted-foreground truncate">{issue.file}</p>}
                    </div>
                  </div>
                ))}
                {sessionIssues.length > 5 && (
                  <p className="text-xs text-muted-foreground text-center pt-1">
                    +{sessionIssues.length - 5} more
                  </p>
                )}
              </div>
            )}
          </div>

          {/* Quick Stats */}
          <div className="card p-3">
            <div className="flex items-center gap-2 mb-2">
              <Activity className="w-4 h-4 text-primary" />
              <span className="text-sm font-medium">
                {currentSession ? "Run Stats" : "All Stats"}
              </span>
            </div>
            <div className="space-y-1 text-xs text-muted-foreground">
              <div className="flex justify-between">
                <span>Messages</span>
                <span className="font-medium text-foreground">{filteredAiOutputLines.length}</span>
              </div>
              <div className="flex justify-between">
                <span>Conversations</span>
                <span className="font-medium text-foreground">{filteredLoops.length}</span>
              </div>
              <div className="flex justify-between">
                <span>Issues Found</span>
                <span className="font-medium text-foreground">{issueSummary.total}</span>
              </div>
              <div className="flex justify-between">
                <span>Resolved</span>
                <span className="font-medium text-green-500">{issueSummary.resolved}</span>
              </div>
            </div>
          </div>

          {/* Help Text when idle */}
          {statusInfo.status === "idle" && (
            <div className="card p-3 bg-muted/30">
              <div className="flex items-start gap-2">
                <Brain className="w-4 h-4 text-primary mt-0.5" />
                <div className="text-xs text-muted-foreground">
                  <p className="font-medium text-foreground mb-1">Getting Started</p>
                  <p>
                    Go to the <strong>Library</strong> tab to run a prompt, workflow, or script. The
                    AI output will appear here in real-time.
                  </p>
                </div>
              </div>
            </div>
          )}
        </div>

        {/* Right Panel - AI Output */}
        <div className="flex-1 min-w-0 flex flex-col p-4">
          <div className="flex-1 min-h-0 card p-4 flex flex-col overflow-hidden">
            <AiOutputTab
              lines={filteredAiOutputLines}
              onClear={onClearAiOutput}
              onAddLine={onAddAiOutputLine}
            />
          </div>
        </div>
      </div>
    </div>
  );
}
