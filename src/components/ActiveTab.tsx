/**
 * ActiveTab.tsx
 *
 * Unified monitoring dashboard that shows real-time status for both
 * GUI automation and AI tasks. This is the "command center" view
 * for active executions.
 *
 * Features:
 * - GUI Automation Panel: Execution status, current state, recent logs
 * - AI Output Panel: Live AI output, task progress, Claude responses
 * - Summary stats, Findings, and Issues panels
 * - Borderless design with subtle backgrounds
 * - Collapsible sections with persistent state
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import {
  AlertTriangle,
  Bot,
  CheckCircle,
  ChevronDown,
  Clock,
  Image,
  Lightbulb,
  Loader2,
  ScrollText,
  Sparkles,
  Square,
} from "lucide-react";
import { useExecution } from "../contexts";
import { AiOutputTab } from "./AiOutputTab";
import type { AiOutputLine } from "./AiOutputTab";
import type { ImageRecognitionEntry } from "../managers";
import ActionLogTable from "./ActionLogTable";
import ImageLogTable from "./ImageLogTable";
import type { ActionLogViewData, ActionLogEntry } from "../types/displayProfile";
import { useUnifiedReport, useFindings } from "../hooks/useUnifiedReport";
import { getSeverityColors, getAccentColors } from "../design-system";

// Storage keys for panel collapse states
const STORAGE_KEY_ACTIONS_COLLAPSED = "activeTab.actionsPanelCollapsed";
const STORAGE_KEY_IMAGE_COLLAPSED = "activeTab.imagePanelCollapsed";
const STORAGE_KEY_FINDINGS_COLLAPSED = "activeTab.findingsPanelCollapsed";

interface ActiveTabProps {
  // Log data
  imageLogs: ImageRecognitionEntry[];
  aiOutputLines: AiOutputLine[];
  actionLogData: ActionLogViewData | null;
  // Clear functions
  onClearAiOutput: () => void;
  // Row click handlers for modals
  onActionRowClick: (action: ActionLogEntry) => void;
  onImageRowClick: (entry: ImageRecognitionEntry) => void;
}

interface ActiveSessionInfo {
  id: string;
  name: string;
  status: string;
  started_at: string;
  uses_gui: boolean;
}

export function ActiveTab({
  imageLogs,
  aiOutputLines,
  actionLogData,
  onClearAiOutput,
  onActionRowClick,
  onImageRowClick,
}: ActiveTabProps) {
  const execution = useExecution();

  // Unified report hooks for summary and findings
  const { summary } = useUnifiedReport();
  const { items: findings } = useFindings();

  // Panel collapse states
  const [actionsCollapsed, setActionsCollapsed] = useState(() => {
    return localStorage.getItem(STORAGE_KEY_ACTIONS_COLLAPSED) === "true";
  });
  const [imageCollapsed, setImageCollapsed] = useState(() => {
    return localStorage.getItem(STORAGE_KEY_IMAGE_COLLAPSED) === "true";
  });
  const [findingsCollapsed, setFindingsCollapsed] = useState(() => {
    return localStorage.getItem(STORAGE_KEY_FINDINGS_COLLAPSED) === "true";
  });

  // AI session state
  const [activeSessions, setActiveSessions] = useState<ActiveSessionInfo[]>([]);
  const [isAiRunning, setIsAiRunning] = useState(false);

  // Persist collapse states
  useEffect(() => {
    localStorage.setItem(STORAGE_KEY_ACTIONS_COLLAPSED, String(actionsCollapsed));
  }, [actionsCollapsed]);

  useEffect(() => {
    localStorage.setItem(STORAGE_KEY_IMAGE_COLLAPSED, String(imageCollapsed));
  }, [imageCollapsed]);

  useEffect(() => {
    localStorage.setItem(STORAGE_KEY_FINDINGS_COLLAPSED, String(findingsCollapsed));
  }, [findingsCollapsed]);

  // Poll for AI session status via HTTP API
  useEffect(() => {
    let interval: ReturnType<typeof setInterval> | null = null;

    const fetchAiStatus = async () => {
      try {
        const response = await fetch("http://localhost:9876/task-runs/running");
        if (response.ok) {
          const tasks = await response.json();
          const runningTasks = Array.isArray(tasks) ? tasks : [];
          setIsAiRunning(runningTasks.length > 0);
          setActiveSessions(
            runningTasks.map(
              (task: { id: string; task_name: string; status: string; created_at: string }) => ({
                id: task.id,
                name: task.task_name,
                status: task.status,
                started_at: task.created_at,
                uses_gui: false,
              }),
            ),
          );
        }
      } catch {
        // Silently ignore - API may not be available
      }
    };

    fetchAiStatus();
    interval = setInterval(fetchAiStatus, 2000);

    return () => {
      if (interval) clearInterval(interval);
    };
  }, []);

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

  // Determine the current run's session ID(s) for filtering
  // Priority: running tasks first, then most recent session from AI output
  const currentRunSessionIds = useMemo(() => {
    // If tasks are running, use their IDs
    if (activeSessions.length > 0) {
      return activeSessions.map((s) => s.id);
    }

    // Otherwise, find the most recent session ID from AI output
    // Look at the latest lines to find their sessionId
    if (aiOutputLines.length > 0) {
      // Get the sessionId from the most recent line that has one
      for (let i = aiOutputLines.length - 1; i >= 0; i--) {
        const line = aiOutputLines[i];
        if (line.sessionId) {
          return [line.sessionId];
        }
      }
    }

    return [];
  }, [activeSessions, aiOutputLines]);

  // Determine the status for the banner
  const statusInfo = useMemo(() => {
    if (isAiRunning && activeSessions.length > 0) {
      const sessionCount = activeSessions.length;
      return {
        status: "running" as const,
        color: "emerald",
        title:
          sessionCount > 1
            ? `${sessionCount} Runs in Progress`
            : activeSessions[0].name || "AI Running",
        subtitle: sessionCount > 1 ? `${sessionCount} runs in progress` : "Processing...",
      };
    }
    if (execution.executionActive) {
      return {
        status: "running" as const,
        color: "emerald",
        title: execution.selectedWorkflow || "Workflow Running",
        subtitle: "GUI automation in progress",
      };
    }
    return {
      status: "idle" as const,
      color: "gray",
      title: "No Active Run",
      subtitle: "Start a task from the Library",
    };
  }, [isAiRunning, activeSessions, execution.executionActive, execution.selectedWorkflow]);

  return (
    <div className="h-full flex flex-col p-4 gap-3 overflow-hidden">
      {/* Status Banner - borderless with subtle background */}
      <div
        className={`flex-shrink-0 px-4 py-3 rounded-lg ${
          statusInfo.status === "running" ? getAccentColors("emerald").bg : "bg-muted/30"
        }`}
      >
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            {statusInfo.status === "running" ? (
              <div
                className={`w-8 h-8 rounded-full ${getAccentColors("emerald").bg} flex items-center justify-center`}
              >
                <Loader2 className={`w-4 h-4 ${getAccentColors("emerald").text} animate-spin`} />
              </div>
            ) : (
              <div className="w-8 h-8 rounded-full bg-muted flex items-center justify-center">
                <Sparkles className="w-4 h-4 text-muted-foreground" />
              </div>
            )}
            <div>
              <h3 className="font-semibold text-foreground text-sm">{statusInfo.title}</h3>
              <p className="text-xs text-muted-foreground">{statusInfo.subtitle}</p>
            </div>
          </div>

          {/* Stop Button - only show when running */}
          {statusInfo.status === "running" && isAiRunning && (
            <button
              onClick={handleStopAi}
              className={`flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-lg transition-colors ${getAccentColors("red").bg} ${getAccentColors("red").text} hover:opacity-80`}
              title="Stop AI analysis"
            >
              <Square className="w-3.5 h-3.5" />
              Stop
            </button>
          )}
        </div>
      </div>

      {/* Summary Stats Row - inline badges without container */}
      {summary.total > 0 && (
        <div className="flex-shrink-0 flex flex-wrap items-center gap-2 px-1">
          <div className="flex items-center gap-1.5 px-2.5 py-1 rounded-md bg-muted/50">
            <span className="text-xs text-muted-foreground">Total</span>
            <span className="text-sm font-semibold">{summary.total}</span>
          </div>
          {summary.resolved > 0 && (
            <div
              className={`flex items-center gap-1.5 px-2.5 py-1 rounded-md ${getAccentColors("green").bg}`}
            >
              <CheckCircle className={`w-3 h-3 ${getAccentColors("green").text}`} />
              <span className={`text-sm font-semibold ${getAccentColors("green").text}`}>
                {summary.resolved}
              </span>
            </div>
          )}
          {summary.actionable > 0 && (
            <div
              className={`flex items-center gap-1.5 px-2.5 py-1 rounded-md ${getAccentColors("yellow").bg}`}
            >
              <Clock className={`w-3 h-3 ${getAccentColors("yellow").text}`} />
              <span className={`text-sm font-semibold ${getAccentColors("yellow").text}`}>
                {summary.actionable}
              </span>
            </div>
          )}
          {summary.bySeverity.critical > 0 && (
            <div
              className={`flex items-center gap-1.5 px-2.5 py-1 rounded-md ${getSeverityColors("critical").bg}`}
            >
              <AlertTriangle className={`w-3 h-3 ${getSeverityColors("critical").text}`} />
              <span className={`text-sm font-semibold ${getSeverityColors("critical").text}`}>
                {summary.bySeverity.critical}
              </span>
            </div>
          )}
          {summary.bySeverity.high > 0 && (
            <div
              className={`flex items-center gap-1.5 px-2.5 py-1 rounded-md ${getSeverityColors("high").bg}`}
            >
              <span className={`text-sm font-semibold ${getSeverityColors("high").text}`}>
                {summary.bySeverity.high} high
              </span>
            </div>
          )}
        </div>
      )}

      {/* Top panels: Actions and Image Recognition side by side */}
      <div className="flex-shrink-0 grid grid-cols-1 lg:grid-cols-2 gap-3">
        {/* Actions Panel - borderless */}
        <div className="rounded-lg bg-card/50">
          <button
            onClick={() => setActionsCollapsed(!actionsCollapsed)}
            className="w-full flex items-center justify-between px-3 py-2 hover:bg-muted/30 rounded-lg transition-colors"
          >
            <div className="flex items-center gap-2">
              <ScrollText className={`w-4 h-4 ${getAccentColors("green").text} opacity-70`} />
              <span className="text-sm font-medium">Actions</span>
              <span className="text-xs text-muted-foreground">
                ({actionLogData?.visible_count || 0})
              </span>
            </div>
            <ChevronDown
              className={`w-4 h-4 text-muted-foreground transition-transform ${!actionsCollapsed && "rotate-180"}`}
            />
          </button>
          {!actionsCollapsed && (
            <div className="max-h-48 overflow-auto px-1 pb-1">
              {actionLogData && actionLogData.actions.length > 0 ? (
                <ActionLogTable
                  actions={actionLogData.actions}
                  onRowClick={onActionRowClick}
                  workflowStartTime={actionLogData.workflow_start_time ?? undefined}
                />
              ) : (
                <div className="py-3 text-center text-muted-foreground text-xs">
                  No actions recorded yet
                </div>
              )}
            </div>
          )}
        </div>

        {/* Image Recognition Panel - borderless */}
        <div className="rounded-lg bg-card/50">
          <button
            onClick={() => setImageCollapsed(!imageCollapsed)}
            className="w-full flex items-center justify-between px-3 py-2 hover:bg-muted/30 rounded-lg transition-colors"
          >
            <div className="flex items-center gap-2">
              <Image className={`w-4 h-4 ${getAccentColors("purple").text} opacity-70`} />
              <span className="text-sm font-medium">Image Recognition</span>
              <span className="text-xs text-muted-foreground">({imageLogs.length})</span>
            </div>
            <ChevronDown
              className={`w-4 h-4 text-muted-foreground transition-transform ${!imageCollapsed && "rotate-180"}`}
            />
          </button>
          {!imageCollapsed && (
            <div className="max-h-48 overflow-auto px-1 pb-1">
              {imageLogs.length > 0 ? (
                <ImageLogTable imageLogs={imageLogs} onRowClick={onImageRowClick} />
              ) : (
                <div className="py-3 text-center text-muted-foreground text-xs">
                  No image recognition results yet
                </div>
              )}
            </div>
          )}
        </div>
      </div>

      {/* Findings and Issues side by side - borderless */}
      <div className="flex-shrink-0 grid grid-cols-1 lg:grid-cols-2 gap-3">
        {/* Findings Panel */}
        <div className="rounded-lg bg-card/50">
          <button
            onClick={() => setFindingsCollapsed(!findingsCollapsed)}
            className="w-full flex items-center justify-between px-3 py-2 hover:bg-muted/30 rounded-lg transition-colors"
          >
            <div className="flex items-center gap-2">
              <Lightbulb className={`w-4 h-4 ${getAccentColors("yellow").text} opacity-70`} />
              <span className="text-sm font-medium">Findings</span>
              <span className="text-xs text-muted-foreground">({findings.length})</span>
            </div>
            <ChevronDown
              className={`w-4 h-4 text-muted-foreground transition-transform ${!findingsCollapsed && "rotate-180"}`}
            />
          </button>
          {!findingsCollapsed && (
            <div className="max-h-40 overflow-auto px-2 pb-2">
              {findings.length > 0 ? (
                <div className="space-y-1">
                  {findings.slice(0, 10).map((finding) => (
                    <div
                      key={finding.id}
                      className="flex items-center gap-2 px-2 py-1.5 rounded bg-muted/30 text-xs"
                    >
                      <span
                        className={`w-1.5 h-1.5 rounded-full ${getSeverityColors(finding.severity).dot}`}
                      />
                      <span className="flex-1 truncate">{finding.title}</span>
                      <span className="text-muted-foreground">{finding.categoryId}</span>
                    </div>
                  ))}
                  {findings.length > 10 && (
                    <div className="text-xs text-muted-foreground text-center py-1">
                      +{findings.length - 10} more
                    </div>
                  )}
                </div>
              ) : (
                <div className="py-3 text-center text-muted-foreground text-xs">
                  No findings yet
                </div>
              )}
            </div>
          )}
        </div>
      </div>

      {/* AI Output - extends to bottom, borderless */}
      <div className="flex-1 flex flex-col min-h-0 rounded-lg bg-card/50 overflow-hidden">
        <div className="flex-shrink-0 flex items-center justify-between px-3 py-2">
          <div className="flex items-center gap-2">
            <Bot className={`w-4 h-4 ${getAccentColors("purple").text} opacity-70`} />
            <span className="text-sm font-medium">AI Output</span>
            {isAiRunning && (
              <Sparkles className={`w-3 h-3 animate-pulse ${getAccentColors("purple").text}`} />
            )}
          </div>
          {activeSessions.length > 0 && (
            <span className="text-xs text-muted-foreground">{activeSessions.length} active</span>
          )}
        </div>
        <div className="flex-1 min-h-0 overflow-auto scrollbar-dark">
          <AiOutputTab
            lines={aiOutputLines}
            onClear={onClearAiOutput}
            filterSessionIds={currentRunSessionIds.length > 0 ? currentRunSessionIds : undefined}
          />
        </div>
      </div>
    </div>
  );
}

export default ActiveTab;
