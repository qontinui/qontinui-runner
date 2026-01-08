/**
 * AiTab.tsx
 *
 * Elevated AI tab component that serves as a top-level navigation item.
 * Provides a comprehensive view of AI sessions with a streamlined layout:
 * - Top: Status banner with controls
 * - Below banner: Session dropdown + quick stats row
 * - Main: Full-width AI Output for maximum visibility
 *
 * This is an elevated version of SessionSubTab from AI Workflows,
 * promoted to be a standalone top-level tab.
 */

import { invoke } from "@tauri-apps/api/core";
import { useState, useEffect, useCallback, useMemo, useRef } from "react";
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
  Brain,
  Activity,
  History,
  Trash2,
  Settings,
  Calendar,
  CheckSquare,
  X,
  Wrench,
  Bot,
} from "lucide-react";
import { AiOutputTab } from "./AiOutputTab";
import type { AiOutputLine } from "./AiOutputTab";
import { useFindings, useUnifiedReport } from "../hooks";
import { groupEntriesIntoLoops, type AiLoop } from "../types/aiLoop";
import { useAutoContinue } from "../contexts";
import { useRunSelectionOptional } from "../contexts/RunSelectionContext";

// localStorage key for stats panel collapse
const STORAGE_KEY_STATS_COLLAPSED = "aiTab.statsCollapsed";

interface AiTabProps {
  aiOutputLines: AiOutputLine[];
  onClearAiOutput: () => void;
  onAddAiOutputLine?: (line: Omit<AiOutputLine, "id">) => void;
  onNavigateToLibrary: () => void;
}

interface ResumableTask {
  name: string;
  sessionsCount: number;
  maxSessions: number | null;
  status: string;
}

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

interface WorkflowSession {
  id: string;
  name: string;
  startTime: number;
  endTime: number;
  loopCount: number;
  isActive: boolean;
}

export function AiTab({
  aiOutputLines,
  onClearAiOutput,
  onAddAiOutputLine,
  onNavigateToLibrary,
}: AiTabProps) {
  // Stats panel collapse state (persisted)
  const [statsCollapsed, setStatsCollapsed] = useState(() => {
    const stored = localStorage.getItem(STORAGE_KEY_STATS_COLLAPSED);
    return stored === "true";
  });

  // Session dropdown state
  const [sessionDropdownOpen, setSessionDropdownOpen] = useState(false);
  const dropdownRef = useRef<HTMLDivElement>(null);

  // Use RunSelectionContext if available
  const runSelection = useRunSelectionOptional();
  const selectedRun = runSelection?.selectedRun;

  // AI running state
  const [isRunning, setIsRunning] = useState(false);
  const [isStopping, setIsStopping] = useState(false);

  // Resumable task state
  const [resumableTask, setResumableTask] = useState<ResumableTask | null>(null);
  const [isResuming, setIsResuming] = useState(false);
  const [isForceContinuing, setIsForceContinuing] = useState(false);

  // Auto-continue settings (from context)
  const {
    enabled: autoContinueEnabled,
    loading: autoContinueLoading,
    toggle: toggleAutoContinue,
  } = useAutoContinue();

  // Auto-fix on failure state
  const [autoFixEnabled, setAutoFixEnabled] = useState(false);
  const [autoFixLoading, setAutoFixLoading] = useState(false);

  // Active task info
  const [activeTask, setActiveTask] = useState<ActiveTaskInfo | null>(null);
  const [activeSessions, setActiveSessions] = useState<ActiveSessionInfo[]>([]);

  // Session/workflow filter
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);

  // Run management (for deletion)
  const [isManagingRuns, setIsManagingRuns] = useState(false);
  const [selectedForDeletion, setSelectedForDeletion] = useState<Set<string>>(new Set());
  const [deleteBeforeDate, setDeleteBeforeDate] = useState<string>("");
  const [isDeleting, setIsDeleting] = useState(false);

  // Findings tracking
  const { items: sessionFindings } = useFindings();
  const { summary: _reportSummary } = useUnifiedReport();

  // Last result message
  const [lastResult, setLastResult] = useState<{ success: boolean; message: string } | null>(null);

  // Per-run auto-continue setting
  const [runAutoContinue, setRunAutoContinue] = useState<boolean | null>(null);
  const [runAutoContinueLoading, setRunAutoContinueLoading] = useState(false);

  // Filter AI output lines by selected run if available from RunSelectionContext
  const runFilteredAiOutputLines = useMemo(() => {
    // If we have a selected run from RunSelectionContext, filter to only show that run's sessions
    if (selectedRun?.id) {
      return aiOutputLines.filter((line) => line.sessionId === selectedRun.id);
    }
    return aiOutputLines;
  }, [aiOutputLines, selectedRun?.id]);

  // Group AI output lines into loops
  const loops = useMemo(
    () => groupEntriesIntoLoops(runFilteredAiOutputLines),
    [runFilteredAiOutputLines],
  );

  // Persist stats collapse state
  useEffect(() => {
    localStorage.setItem(STORAGE_KEY_STATS_COLLAPSED, String(statsCollapsed));
  }, [statsCollapsed]);

  // Close dropdown when clicking outside
  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (dropdownRef.current && !dropdownRef.current.contains(event.target as Node)) {
        setSessionDropdownOpen(false);
      }
    };
    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, []);

  // Extract workflow sessions from loops
  const workflowSessions = useMemo(() => {
    const sessions: WorkflowSession[] = [];

    const findBestLoopName = (loop: AiLoop): string | null => {
      if (loop.sessionName && loop.sessionName !== "AI Response") {
        return loop.sessionName;
      }
      if (loop.promptPreview && loop.promptPreview !== "AI Response") {
        return loop.promptPreview;
      }
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

    interface TempSession {
      id: string;
      loops: AiLoop[];
      startTime: number;
      endTime: number;
    }
    const sessionMap = new Map<string, TempSession>();

    for (const loop of loops) {
      const sessionId = loop.sessionId || loop.id;

      if (sessionMap.has(sessionId)) {
        const session = sessionMap.get(sessionId)!;
        session.loops.push(loop);
        session.endTime = Math.max(session.endTime, loop.endTime);
        session.startTime = Math.min(session.startTime, loop.startTime);
      } else {
        sessionMap.set(sessionId, {
          id: sessionId,
          loops: [loop],
          startTime: loop.startTime,
          endTime: loop.endTime,
        });
      }
    }

    for (const temp of sessionMap.values()) {
      let bestName: string | null = null;

      for (const loop of temp.loops) {
        if (loop.sessionName && loop.sessionName !== "AI Response") {
          bestName = loop.sessionName;
          break;
        }
      }

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

      if (!bestName) {
        for (const loop of temp.loops) {
          const loopName = findBestLoopName(loop);
          if (loopName) {
            bestName = loopName;
            break;
          }
        }
      }

      if (!bestName) {
        bestName = "Untitled Run";
      }

      sessions.push({
        id: temp.id,
        name: bestName,
        startTime: temp.startTime,
        endTime: temp.endTime,
        loopCount: temp.loops.length,
        isActive: activeSessions.some((s) => s.id === temp.id),
      });
    }

    return sessions.sort((a, b) => b.endTime - a.endTime);
  }, [loops, activeSessions]);

  // Group sessions by date
  const sessionsByDate = useMemo(() => {
    const groups: Map<string, WorkflowSession[]> = new Map();
    const today = new Date();
    const yesterday = new Date(today);
    yesterday.setDate(yesterday.getDate() - 1);

    for (const session of workflowSessions) {
      const date = new Date(session.startTime);
      let dateKey: string;

      if (date.toDateString() === today.toDateString()) {
        dateKey = "Today";
      } else if (date.toDateString() === yesterday.toDateString()) {
        dateKey = "Yesterday";
      } else {
        dateKey = date.toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" });
      }

      if (!groups.has(dateKey)) {
        groups.set(dateKey, []);
      }
      groups.get(dateKey)!.push(session);
    }

    return groups;
  }, [workflowSessions]);

  // Get the selected session or default to the most recent
  const currentSession = useMemo(() => {
    if (!selectedSessionId && workflowSessions.length > 0) {
      return workflowSessions[0];
    }
    return workflowSessions.find((s) => s.id === selectedSessionId) || null;
  }, [selectedSessionId, workflowSessions]);

  // Filter loops by selected session
  const filteredLoops = useMemo(() => {
    if (!currentSession) return loops;
    return loops.filter((loop) => loop.sessionId === currentSession.id);
  }, [loops, currentSession]);

  // Get filtered AI output lines based on selected session
  const filteredAiOutputLines = useMemo(() => {
    if (!currentSession) return runFilteredAiOutputLines;

    const filteredEntryIds = new Set<string>();
    for (const loop of filteredLoops) {
      for (const entry of loop.entries) {
        filteredEntryIds.add(entry.id);
      }
    }

    return runFilteredAiOutputLines.filter((line) => filteredEntryIds.has(line.id));
  }, [runFilteredAiOutputLines, currentSession, filteredLoops]);

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
          if (result.data?.is_running !== undefined) {
            setIsRunning(result.data.is_running);
          }
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

    checkStatus();
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
  const handleForceContinue = useCallback(async () => {
    if (isForceContinuing) return;

    setIsForceContinuing(true);
    try {
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
  }, []);

  // Toggle run management mode
  const handleToggleManagement = useCallback(() => {
    setIsManagingRuns((prev) => {
      if (prev) {
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
      cutoffDate.setHours(23, 59, 59, 999);
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

  // Delete selected runs
  const handleDeleteRuns = useCallback(async () => {
    if (selectedForDeletion.size === 0) return;

    setIsDeleting(true);

    try {
      const sessionIds = Array.from(selectedForDeletion);

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

      if (selectedForDeletion.size === workflowSessions.length) {
        await invoke("clear_ai_output_log");
        onClearAiOutput();
      } else {
        await invoke("clear_ai_output_log");
        onClearAiOutput();
      }
    } catch (error) {
      console.error("Failed to delete runs:", error);
    } finally {
      setSelectedForDeletion(new Set());
      setDeleteBeforeDate("");
      setIsManagingRuns(false);
      setIsDeleting(false);
    }
  }, [selectedForDeletion, workflowSessions, onClearAiOutput]);

  // Select/deselect all sessions
  const handleSelectAll = useCallback(() => {
    if (selectedForDeletion.size === workflowSessions.length) {
      setSelectedForDeletion(new Set());
    } else {
      setSelectedForDeletion(new Set(workflowSessions.map((s) => s.id)));
    }
  }, [workflowSessions, selectedForDeletion.size]);

  // Get findings summary
  const findingsSummary = useMemo(() => {
    const summary = {
      total: sessionFindings.length,
      detected: 0,
      inProgress: 0,
      resolved: 0,
      critical: 0,
    };
    for (const finding of sessionFindings) {
      if (finding.status === "detected") summary.detected++;
      if (finding.status === "in_progress") summary.inProgress++;
      if (finding.status === "resolved") summary.resolved++;
      if (finding.severity === "critical") summary.critical++;
    }
    return summary;
  }, [sessionFindings]);

  // Determine the status for the banner
  const statusInfo = useMemo(() => {
    if (isRunning) {
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

  // Toggle stats collapse
  const _toggleStats = () => setStatsCollapsed((prev) => !prev);

  // No run selected state (when context is present but no run selected)
  // NOTE: This check must come AFTER all hooks to satisfy React's rules of hooks
  if (runSelection && !selectedRun) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground p-8">
        <Bot className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No Run Selected</p>
        <p className="text-sm mt-2 text-center max-w-md">
          Select a run from the Run Dashboard to view AI output.
        </p>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Status Banner */}
      <div
        className={`flex-shrink-0 px-4 py-3 border-b ${
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
              <div className="w-9 h-9 rounded-full bg-emerald-500/20 flex items-center justify-center">
                <Loader2 className="w-4 h-4 text-emerald-500 animate-spin" />
              </div>
            ) : statusInfo.status === "resumable" ? (
              <div className="w-9 h-9 rounded-full bg-orange-500/20 flex items-center justify-center">
                <Clock className="w-4 h-4 text-orange-500" />
              </div>
            ) : (
              <div className="w-9 h-9 rounded-full bg-muted flex items-center justify-center">
                <Sparkles className="w-4 h-4 text-muted-foreground" />
              </div>
            )}
            <div>
              <h3 className="font-semibold text-foreground text-sm">{statusInfo.title}</h3>
              <p className="text-xs text-muted-foreground">{statusInfo.subtitle}</p>
            </div>
          </div>

          {/* Action Buttons and Toggles */}
          <div className="flex items-center gap-3">
            {/* Active Sessions List */}
            {activeSessions.length > 1 && (
              <div className="flex items-center gap-2 text-xs">
                {activeSessions.slice(0, 3).map((session) => (
                  <span
                    key={session.id}
                    className={`px-2 py-1 rounded-full ${
                      session.uses_gui
                        ? "bg-purple-500/20 text-purple-400"
                        : "bg-blue-500/20 text-blue-400"
                    }`}
                    title={session.uses_gui ? "GUI Session" : "Non-GUI Session"}
                  >
                    {session.name.length > 15
                      ? session.name.substring(0, 12) + "..."
                      : session.name}
                  </span>
                ))}
                {activeSessions.length > 3 && (
                  <span className="text-muted-foreground">+{activeSessions.length - 3}</span>
                )}
              </div>
            )}

            {/* Auto-Continue Toggle */}
            <button
              onClick={toggleAutoContinue}
              disabled={autoContinueLoading}
              className={`flex items-center gap-1.5 px-2 py-1 rounded text-xs transition-colors ${
                autoContinueEnabled
                  ? "bg-orange-500/20 text-orange-400 hover:bg-orange-500/30"
                  : "bg-muted text-muted-foreground hover:bg-muted/80"
              } ${autoContinueLoading ? "opacity-50" : ""}`}
              title={autoContinueEnabled ? "Auto-continue enabled" : "Auto-continue disabled"}
            >
              <RotateCcw className="w-3 h-3" />
              {autoContinueLoading ? (
                <Loader2 className="w-3 h-3 animate-spin" />
              ) : autoContinueEnabled ? (
                <ToggleRight className="w-4 h-4" />
              ) : (
                <ToggleLeft className="w-4 h-4" />
              )}
            </button>

            {/* Auto-Fix Toggle */}
            <button
              onClick={toggleAutoFix}
              disabled={autoFixLoading}
              className={`flex items-center gap-1.5 px-2 py-1 rounded text-xs transition-colors ${
                autoFixEnabled
                  ? "bg-blue-500/20 text-blue-400 hover:bg-blue-500/30"
                  : "bg-muted text-muted-foreground hover:bg-muted/80"
              } ${autoFixLoading ? "opacity-50" : ""}`}
              title={autoFixEnabled ? "Auto-fix enabled" : "Auto-fix disabled"}
            >
              <Wrench className="w-3 h-3" />
              {autoFixLoading ? (
                <Loader2 className="w-3 h-3 animate-spin" />
              ) : autoFixEnabled ? (
                <ToggleRight className="w-4 h-4" />
              ) : (
                <ToggleLeft className="w-4 h-4" />
              )}
            </button>

            {/* Continue Button - When resumable */}
            {resumableTask && !isRunning && (
              <button
                onClick={handleResumeWorkflow}
                disabled={isResuming}
                className="flex items-center gap-2 px-3 py-1.5 bg-orange-500 text-white rounded-lg font-medium text-sm hover:bg-orange-600 disabled:opacity-50 transition-colors"
              >
                {isResuming ? (
                  <>
                    <Loader2 className="w-3.5 h-3.5 animate-spin" />
                    Resuming...
                  </>
                ) : (
                  <>
                    <Play className="w-3.5 h-3.5" />
                    Continue
                  </>
                )}
              </button>
            )}

            {/* Continue Button - When stopped unexpectedly */}
            {!isRunning && !resumableTask && runFilteredAiOutputLines.length > 0 && (
              <button
                onClick={handleForceContinue}
                disabled={isForceContinuing}
                className="flex items-center gap-2 px-3 py-1.5 bg-amber-600 text-white rounded-lg font-medium text-sm hover:bg-amber-700 disabled:opacity-50 transition-colors"
                title="Continue a run that stopped. Uses recent output as context."
              >
                {isForceContinuing ? (
                  <>
                    <Loader2 className="w-3.5 h-3.5 animate-spin" />
                    Starting...
                  </>
                ) : (
                  <>
                    <RotateCcw className="w-3.5 h-3.5" />
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
                className="flex items-center gap-2 px-3 py-1.5 bg-red-500 text-white rounded-lg font-medium text-sm hover:bg-red-600 disabled:opacity-50 transition-colors"
              >
                {isStopping ? (
                  <>
                    <Loader2 className="w-3.5 h-3.5 animate-spin" />
                    Stopping...
                  </>
                ) : (
                  <>
                    <Square className="w-3.5 h-3.5" />
                    Stop
                  </>
                )}
              </button>
            )}

            {/* Go to Library - When idle */}
            {statusInfo.status === "idle" && (
              <button
                onClick={onNavigateToLibrary}
                className="flex items-center gap-2 px-3 py-1.5 bg-primary text-primary-foreground rounded-lg font-medium text-sm hover:bg-primary/90 transition-colors"
              >
                <Sparkles className="w-3.5 h-3.5" />
                Start from Library
              </button>
            )}
          </div>
        </div>

        {/* Last Result Message */}
        {lastResult && (
          <div
            className={`mt-2 flex items-center gap-2 text-xs ${
              lastResult.success ? "text-emerald-400" : "text-red-400"
            }`}
          >
            {lastResult.success ? (
              <CheckCircle className="w-3.5 h-3.5" />
            ) : (
              <XCircle className="w-3.5 h-3.5" />
            )}
            {lastResult.message}
          </div>
        )}
      </div>

      {/* Session Dropdown + Stats Row */}
      <div className="flex-shrink-0 px-4 py-2 border-b border-border bg-muted/20">
        <div className="flex items-center gap-4">
          {/* Session Dropdown */}
          <div className="relative" ref={dropdownRef}>
            <button
              onClick={() => setSessionDropdownOpen(!sessionDropdownOpen)}
              className="flex items-center gap-2 px-3 py-1.5 bg-background border border-border rounded-lg hover:bg-muted/50 transition-colors min-w-[200px]"
            >
              <History className="w-4 h-4 text-primary" />
              <span className="flex-1 text-left text-sm truncate">
                {currentSession ? currentSession.name : "All Sessions"}
              </span>
              <span className="text-xs text-muted-foreground">({workflowSessions.length})</span>
              <ChevronDown
                className={`w-4 h-4 transition-transform ${sessionDropdownOpen ? "rotate-180" : ""}`}
              />
            </button>

            {/* Dropdown Menu */}
            {sessionDropdownOpen && (
              <div className="absolute z-50 top-full left-0 mt-1 w-80 max-h-80 overflow-y-auto bg-card border border-border rounded-lg shadow-lg">
                {/* Management Toggle */}
                <div className="flex items-center justify-between px-3 py-2 border-b border-border bg-muted/30">
                  <span className="text-xs font-medium text-muted-foreground">Sessions</span>
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      handleToggleManagement();
                    }}
                    className="p-1 text-muted-foreground hover:text-foreground hover:bg-muted/50 rounded transition-colors"
                    title={isManagingRuns ? "Cancel" : "Manage runs"}
                  >
                    {isManagingRuns ? (
                      <X className="w-3.5 h-3.5" />
                    ) : (
                      <Settings className="w-3.5 h-3.5" />
                    )}
                  </button>
                </div>

                {/* Management Controls */}
                {isManagingRuns && (
                  <div className="px-3 py-2 border-b border-border bg-muted/20 space-y-2">
                    <div className="flex items-center gap-2">
                      <Calendar className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                      <input
                        type="date"
                        value={deleteBeforeDate}
                        onChange={(e) => handleSelectBeforeDate(e.target.value)}
                        onClick={(e) => e.stopPropagation()}
                        className="flex-1 text-xs bg-background border border-border rounded px-2 py-1"
                        title="Select runs before this date"
                      />
                    </div>
                    <div className="flex items-center gap-2">
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          handleSelectAll();
                        }}
                        className="flex items-center gap-1 px-2 py-1 text-xs bg-muted hover:bg-muted/80 rounded transition-colors"
                      >
                        <CheckSquare className="w-3 h-3" />
                        {selectedForDeletion.size === workflowSessions.length
                          ? "Deselect"
                          : "Select All"}
                      </button>
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          handleDeleteRuns();
                        }}
                        disabled={selectedForDeletion.size === 0 || isDeleting}
                        className="flex items-center gap-1 px-2 py-1 text-xs bg-red-500/20 text-red-400 hover:bg-red-500/30 rounded transition-colors disabled:opacity-50"
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

                {/* All Sessions Option */}
                {!isManagingRuns && workflowSessions.length > 0 && (
                  <button
                    onClick={() => {
                      handleSelectSession(null);
                      setSessionDropdownOpen(false);
                    }}
                    className={`w-full flex items-center gap-2 px-3 py-2 text-left text-sm hover:bg-muted/50 transition-colors ${
                      !selectedSessionId ? "bg-primary/10" : ""
                    }`}
                  >
                    <Brain className="w-4 h-4 text-primary" />
                    <span className="flex-1">All Sessions</span>
                    <span className="text-xs text-muted-foreground">{workflowSessions.length}</span>
                  </button>
                )}

                {/* Sessions grouped by date */}
                {Array.from(sessionsByDate.entries()).map(([dateLabel, sessions]) => (
                  <div key={dateLabel}>
                    <div className="px-3 py-1 text-xs font-medium text-muted-foreground bg-muted/30 sticky top-0">
                      {dateLabel}
                    </div>
                    {sessions.map((session) => (
                      <div
                        key={session.id}
                        onClick={() => {
                          if (isManagingRuns) {
                            handleToggleSessionSelection(session.id);
                          } else {
                            handleSelectSession(session.id);
                            setSessionDropdownOpen(false);
                          }
                        }}
                        className={`flex items-center gap-2 px-3 py-2 text-sm hover:bg-muted/50 transition-colors cursor-pointer ${
                          isManagingRuns
                            ? selectedForDeletion.has(session.id)
                              ? "bg-red-500/10"
                              : ""
                            : selectedSessionId === session.id
                              ? "bg-primary/10"
                              : ""
                        }`}
                      >
                        {isManagingRuns ? (
                          <input
                            type="checkbox"
                            checked={selectedForDeletion.has(session.id)}
                            onChange={() => handleToggleSessionSelection(session.id)}
                            onClick={(e) => e.stopPropagation()}
                            className="flex-shrink-0 accent-red-500"
                          />
                        ) : (
                          <Brain className="w-3.5 h-3.5 text-primary flex-shrink-0" />
                        )}
                        <div className="flex-1 min-w-0">
                          <div className="flex items-center gap-1.5">
                            <span className="font-medium truncate">{session.name}</span>
                            {session.isActive && (
                              <span className="text-[10px] px-1 py-0.5 bg-emerald-500/20 text-emerald-400 rounded flex-shrink-0">
                                Active
                              </span>
                            )}
                          </div>
                          <div className="text-xs text-muted-foreground">
                            {formatTime(session.startTime)} • {session.loopCount} conv
                            {session.loopCount !== 1 ? "s" : ""}
                          </div>
                        </div>
                      </div>
                    ))}
                  </div>
                ))}

                {workflowSessions.length === 0 && (
                  <div className="px-3 py-4 text-center text-sm text-muted-foreground">
                    No sessions yet
                  </div>
                )}
              </div>
            )}
          </div>

          {/* Quick Stats (inline) */}
          <div className="flex items-center gap-4 text-xs">
            <div className="flex items-center gap-1.5">
              <Activity className="w-3.5 h-3.5 text-primary" />
              <span className="text-muted-foreground">Messages:</span>
              <span className="font-medium">{filteredAiOutputLines.length}</span>
            </div>
            <div className="flex items-center gap-1.5">
              <Brain className="w-3.5 h-3.5 text-primary" />
              <span className="text-muted-foreground">Convs:</span>
              <span className="font-medium">{filteredLoops.length}</span>
            </div>
            {findingsSummary.total > 0 ? (
              <div className="flex items-center gap-1.5">
                <AlertTriangle
                  className={`w-3.5 h-3.5 ${findingsSummary.critical > 0 ? "text-red-500" : "text-orange-500"}`}
                />
                <span className="text-muted-foreground">Findings:</span>
                <span className="font-medium">{findingsSummary.total}</span>
                {findingsSummary.resolved > 0 && (
                  <span className="text-green-500">({findingsSummary.resolved} fixed)</span>
                )}
              </div>
            ) : (
              <div className="flex items-center gap-1.5">
                <CheckCircle className="w-3.5 h-3.5 text-green-500" />
                <span className="text-muted-foreground">No findings</span>
              </div>
            )}
          </div>

          {/* Per-Run Auto-Continue (if selected) */}
          {selectedSessionId && runAutoContinue !== null && (
            <div className="ml-auto flex items-center gap-2">
              <button
                onClick={toggleRunAutoContinue}
                disabled={runAutoContinueLoading}
                className={`flex items-center gap-1.5 px-2 py-1 rounded text-xs transition-colors ${
                  runAutoContinue
                    ? "bg-purple-500/20 text-purple-400 hover:bg-purple-500/30"
                    : "bg-muted text-muted-foreground hover:bg-muted/80"
                } ${runAutoContinueLoading ? "opacity-50" : ""}`}
                title={
                  runAutoContinue
                    ? "Auto-continue enabled for this run"
                    : "Auto-continue disabled for this run"
                }
              >
                <Play className="w-3 h-3" />
                {runAutoContinueLoading ? (
                  <Loader2 className="w-3 h-3 animate-spin" />
                ) : runAutoContinue ? (
                  <ToggleRight className="w-4 h-4" />
                ) : (
                  <ToggleLeft className="w-4 h-4" />
                )}
              </button>
            </div>
          )}
        </div>
      </div>

      {/* Main Content - Full Width AI Output */}
      <div className="flex-1 min-h-0 flex flex-col p-3 overflow-hidden">
        <div className="flex-1 min-h-0 card p-3 flex flex-col overflow-hidden">
          <AiOutputTab
            lines={filteredAiOutputLines}
            onClear={onClearAiOutput}
            onAddLine={onAddAiOutputLine}
          />
        </div>
      </div>
    </div>
  );
}
