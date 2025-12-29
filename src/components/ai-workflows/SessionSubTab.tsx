/**
 * SessionSubTab.tsx
 *
 * Unified AI Session view that shows:
 * - Current AI status (Running/Idle/Resumable) at the top
 * - Workflow/session selector to filter loops by automation
 * - Continue button prominently when a workflow is resumable
 * - Split panel: Quick actions on left, Live AI Output on right
 * - Stop button when AI is running
 *
 * This is the primary view users should see when AI is active.
 */

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

interface ResumableWorkflow {
  name: string;
  currentPhase: number;
  totalPhases: number;
  status: string;
}

interface ActiveWorkflowInfo {
  name: string;
  type: "prompt" | "workflow" | "builder";
  startedAt?: string;
  iteration?: number;
  maxIterations?: number;
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

export function SessionSubTab({
  aiOutputLines,
  onClearAiOutput,
  onAddAiOutputLine,
  onNavigateToLibrary,
}: SessionSubTabProps) {
  // AI running state
  const [isRunning, setIsRunning] = useState(false);
  const [isStopping, setIsStopping] = useState(false);

  // Resumable workflow state
  const [resumableWorkflow, setResumableWorkflow] = useState<ResumableWorkflow | null>(null);
  const [isResuming, setIsResuming] = useState(false);

  // Auto-continue settings (from context - syncs across all components)
  const {
    enabled: autoContinueEnabled,
    loading: autoContinueLoading,
    toggle: toggleAutoContinue,
  } = useAutoContinue();

  // Auto-fix on failure state
  const [autoFixEnabled, setAutoFixEnabled] = useState(false);
  const [autoFixLoading, setAutoFixLoading] = useState(false);

  // Active workflow info (when running)
  const [activeWorkflow, setActiveWorkflow] = useState<ActiveWorkflowInfo | null>(null);

  // Session/workflow filter
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);
  const [showSessionSelector, setShowSessionSelector] = useState(false);

  // Session management (for deletion)
  const [isManagingsessions, setIsManagingessions] = useState(false);
  const [selectedForDeletion, setSelectedForDeletion] = useState<Set<string>>(new Set());
  const [deleteBeforeDate, setDeleteBeforeDate] = useState<string>("");
  const [isDeleting, setIsDeleting] = useState(false);

  // Issues tracking
  const [sessionIssues, setSessionIssues] = useState<DetectedIssue[]>([]);
  const [showIssues, setShowIssues] = useState(true);

  // Last result message
  const [lastResult, setLastResult] = useState<{ success: boolean; message: string } | null>(null);

  // Group AI output lines into loops
  const loops = useMemo(() => groupEntriesIntoLoops(aiOutputLines), [aiOutputLines]);

  // Extract workflow sessions from loops
  // Sessions are inferred from gaps between loops (> 5 minutes = new session)
  // or from explicit sessionName/sessionId in the entries
  const workflowSessions = useMemo(() => {
    const sessions: WorkflowSession[] = [];
    const SESSION_GAP_MS = 5 * 60 * 1000; // 5 minutes

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

    // First pass: group loops into sessions based on time gaps
    interface TempSession {
      id: string;
      loops: AiLoop[];
      startTime: number;
      endTime: number;
    }
    const tempSessions: TempSession[] = [];
    let currentTemp: TempSession | null = null;
    let lastLoopEndTime = 0;

    for (const loop of loops) {
      const sessionId = loop.sessionId || `session-${loop.startTime}`;
      const timeSinceLast = loop.startTime - lastLoopEndTime;
      const shouldStartNewSession =
        !currentTemp ||
        timeSinceLast > SESSION_GAP_MS ||
        (loop.sessionId && loop.sessionId !== currentTemp.id);

      if (shouldStartNewSession) {
        if (currentTemp) {
          tempSessions.push(currentTemp);
        }
        currentTemp = {
          id: sessionId,
          loops: [loop],
          startTime: loop.startTime,
          endTime: loop.endTime,
        };
      } else if (currentTemp) {
        currentTemp.loops.push(loop);
        currentTemp.endTime = loop.endTime;
      }

      lastLoopEndTime = loop.endTime;
    }

    if (currentTemp) {
      tempSessions.push(currentTemp);
    }

    // Second pass: find the best name for each session by looking through all its loops
    for (const temp of tempSessions) {
      let bestName: string | null = null;

      // Look through loops in order to find first meaningful name
      for (const loop of temp.loops) {
        const loopName = findBestLoopName(loop);
        if (loopName) {
          bestName = loopName;
          break;
        }
      }

      // Fallback to a generic name with timestamp
      if (!bestName) {
        const date = new Date(temp.startTime);
        bestName = `Session at ${date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;
      }

      const now = Date.now();
      sessions.push({
        id: temp.id,
        name: bestName,
        startTime: temp.startTime,
        endTime: temp.endTime,
        loopCount: temp.loops.length,
        isActive: isRunning && now - temp.endTime < SESSION_GAP_MS,
      });
    }

    return sessions.reverse(); // Most recent first
  }, [loops, isRunning]);

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

  // Check for resumable workflows and running state periodically
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
          // Only show Continue if there's a resumable workflow AND nothing is running
          if (result.data?.has_resumable && !result.data?.is_running) {
            setResumableWorkflow({
              name: result.data.name || "Unknown Workflow",
              currentPhase: result.data.current_phase || 0,
              totalPhases: result.data.total_phases || 0,
              status: result.data.status || "unknown",
            });
          } else {
            setResumableWorkflow(null);
          }

          // Set active workflow info when running
          if (result.data?.is_running && result.data?.name) {
            setActiveWorkflow({
              name: result.data.name,
              type: result.data.workflow_type || "workflow",
              iteration: result.data.current_phase,
              maxIterations: result.data.total_phases,
            });
          } else if (!result.data?.is_running) {
            setActiveWorkflow(null);
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
    if (!resumableWorkflow || isResuming) return;

    setIsResuming(true);
    try {
      const response = await fetch("http://localhost:9876/workflow/resume", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
      });
      const result = await response.json();

      if (result.success) {
        setResumableWorkflow(null);
        setIsRunning(true);
        setLastResult({ success: true, message: `Resuming workflow: ${resumableWorkflow.name}` });
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
  }, [resumableWorkflow, isResuming]);

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

  // Toggle session management mode
  const handleToggleManagement = useCallback(() => {
    setIsManagingessions((prev) => {
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

  // Delete selected sessions
  const handleDeleteSessions = useCallback(() => {
    if (selectedForDeletion.size === 0) return;

    setIsDeleting(true);

    // Get the session IDs to delete and find the corresponding loop entries
    const sessionIds = Array.from(selectedForDeletion);
    const sessionsToDelete = workflowSessions.filter((s) => sessionIds.includes(s.id));

    // Find all entry IDs that belong to these sessions (by time range)
    const entryIdsToDelete = new Set<string>();
    for (const session of sessionsToDelete) {
      const BUFFER_MS = 1000;
      for (const line of aiOutputLines) {
        if (
          line.timestamp >= session.startTime - BUFFER_MS &&
          line.timestamp <= session.endTime + BUFFER_MS
        ) {
          entryIdsToDelete.add(line.id);
        }
      }
    }

    // Call the clear function with the IDs to delete
    // Since onClearAiOutput clears all, we need to filter
    // For now, we'll just clear all if deleting - proper implementation would need backend support
    if (entryIdsToDelete.size > 0) {
      onClearAiOutput();
    }

    // Reset state
    setSelectedForDeletion(new Set());
    setDeleteBeforeDate("");
    setIsManagingessions(false);
    setIsDeleting(false);
  }, [selectedForDeletion, workflowSessions, aiOutputLines, onClearAiOutput]);

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
      return {
        status: "running" as const,
        color: "emerald",
        icon: Loader2,
        title: activeWorkflow?.name || "AI Running",
        subtitle: activeWorkflow
          ? `${activeWorkflow.type} - Phase ${activeWorkflow.iteration || "?"} of ${activeWorkflow.maxIterations || "?"}`
          : "Processing...",
      };
    }
    if (resumableWorkflow) {
      return {
        status: "resumable" as const,
        color: "orange",
        icon: Clock,
        title: resumableWorkflow.name,
        subtitle: `Paused at Phase ${resumableWorkflow.currentPhase} of ${resumableWorkflow.totalPhases}`,
      };
    }
    return {
      status: "idle" as const,
      color: "gray",
      icon: Sparkles,
      title: "No Active Session",
      subtitle: "Start a prompt or workflow from the Library",
    };
  }, [isRunning, resumableWorkflow, activeWorkflow]);

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
            {/* Continue Button - Prominent when resumable */}
            {resumableWorkflow && !isRunning && (
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
                    Continue Workflow
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
          {/* Session Selector */}
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
                      {isManagingsessions
                        ? "Manage Sessions"
                        : currentSession
                          ? currentSession.name.length > 15
                            ? currentSession.name.substring(0, 15) + "..."
                            : currentSession.name
                          : "All Sessions"}
                    </span>
                    <span className="text-xs text-muted-foreground">
                      {isManagingsessions
                        ? `${selectedForDeletion.size} selected`
                        : currentSession
                          ? `${currentSession.loopCount} loops`
                          : `${workflowSessions.length} sessions`}
                    </span>
                  </div>
                </button>
                <div className="flex items-center gap-1">
                  {!isManagingsessions ? (
                    <button
                      onClick={handleToggleManagement}
                      className="p-1.5 text-muted-foreground hover:text-foreground hover:bg-muted/50 rounded transition-colors"
                      title="Manage sessions"
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
              {isManagingsessions && showSessionSelector && (
                <div className="px-3 py-2 border-b border-border bg-muted/30 space-y-2">
                  {/* Date filter */}
                  <div className="flex items-center gap-2">
                    <Calendar className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                    <input
                      type="date"
                      value={deleteBeforeDate}
                      onChange={(e) => handleSelectBeforeDate(e.target.value)}
                      className="flex-1 text-xs bg-background border border-border rounded px-2 py-1"
                      title="Select sessions before this date"
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
                      onClick={handleDeleteSessions}
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

              {/* Session List */}
              {showSessionSelector && (
                <div className="px-2 pb-2 pt-1 space-y-1 max-h-48 overflow-y-auto">
                  {/* All Sessions option (only in view mode) */}
                  {!isManagingsessions && (
                    <button
                      onClick={() => handleSelectSession(null)}
                      className={`w-full flex items-center gap-2 p-2 rounded text-left text-sm hover:bg-muted/50 transition-colors ${
                        !selectedSessionId ? "bg-muted" : ""
                      }`}
                    >
                      <Filter className="w-3 h-3 text-muted-foreground" />
                      <span>All Sessions</span>
                      <span className="text-xs text-muted-foreground ml-auto">
                        {loops.length} loops
                      </span>
                    </button>
                  )}

                  {/* Individual sessions */}
                  {workflowSessions.map((session) => (
                    <div
                      key={session.id}
                      className={`flex items-start gap-2 p-2 rounded text-sm hover:bg-muted/50 transition-colors ${
                        isManagingsessions
                          ? selectedForDeletion.has(session.id)
                            ? "bg-red-500/10"
                            : ""
                          : selectedSessionId === session.id
                            ? "bg-muted"
                            : ""
                      }`}
                    >
                      {isManagingsessions ? (
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
                              {session.loopCount} loops
                            </div>
                          </div>
                        </button>
                      )}
                      {isManagingsessions && (
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
                              {formatDate(session.startTime)} - {session.loopCount} loops
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

          {/* Auto-Continue Toggle */}
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
                {currentSession ? "Session Stats" : "All Stats"}
              </span>
            </div>
            <div className="space-y-1 text-xs text-muted-foreground">
              <div className="flex justify-between">
                <span>Messages</span>
                <span className="font-medium text-foreground">{filteredAiOutputLines.length}</span>
              </div>
              <div className="flex justify-between">
                <span>Loops</span>
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
