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
 * - Always accessible - shows appropriate content based on what's running
 * - Remembers last known state for each panel
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import {
  Bot,
  ChevronDown,
  ChevronUp,
  Image,
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

// Storage keys for panel collapse states
const STORAGE_KEY_GUI_COLLAPSED = "activeTab.guiPanelCollapsed";
const STORAGE_KEY_AI_COLLAPSED = "activeTab.aiPanelCollapsed";

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

  // Panel collapse states
  const [guiPanelCollapsed, setGuiPanelCollapsed] = useState(() => {
    return localStorage.getItem(STORAGE_KEY_GUI_COLLAPSED) === "true";
  });
  const [aiPanelCollapsed, setAiPanelCollapsed] = useState(() => {
    return localStorage.getItem(STORAGE_KEY_AI_COLLAPSED) === "true";
  });

  // AI session state
  const [activeSessions, setActiveSessions] = useState<ActiveSessionInfo[]>([]);
  const [isAiRunning, setIsAiRunning] = useState(false);

  // Persist collapse states
  useEffect(() => {
    localStorage.setItem(STORAGE_KEY_GUI_COLLAPSED, String(guiPanelCollapsed));
  }, [guiPanelCollapsed]);

  useEffect(() => {
    localStorage.setItem(STORAGE_KEY_AI_COLLAPSED, String(aiPanelCollapsed));
  }, [aiPanelCollapsed]);

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
    <div className="h-full flex flex-col p-4 gap-4 overflow-hidden">
      {/* Status Banner */}
      <div
        className={`flex-shrink-0 px-4 py-3 rounded-lg ${
          statusInfo.status === "running"
            ? "bg-emerald-500/10 border border-emerald-500/30"
            : "bg-muted/30 border border-border"
        }`}
      >
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            {statusInfo.status === "running" ? (
              <div className="w-9 h-9 rounded-full bg-emerald-500/20 flex items-center justify-center">
                <Loader2 className="w-4 h-4 text-emerald-500 animate-spin" />
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

          {/* Stop Button - only show when running */}
          {statusInfo.status === "running" && isAiRunning && (
            <button
              onClick={handleStopAi}
              className="flex items-center gap-1.5 px-3 py-1.5 text-sm bg-red-500/20 text-red-400 hover:bg-red-500/30 rounded-lg transition-colors"
              title="Stop AI analysis"
            >
              <Square className="w-3.5 h-3.5" />
              Stop
            </button>
          )}
        </div>
      </div>

      {/* Top panels: Actions and Image Recognition side by side */}
      <div className="flex-shrink-0 grid grid-cols-1 lg:grid-cols-2 gap-4">
        {/* Actions Panel */}
        <div className="card">
          <button
            onClick={() => setGuiPanelCollapsed(!guiPanelCollapsed)}
            className="w-full flex items-center justify-between p-3 hover:bg-muted/30 transition-colors"
          >
            <div className="flex items-center gap-2">
              <ScrollText className="w-4 h-4 text-green-500" />
              <h2 className="font-medium text-sm">Actions</h2>
              <span className="text-xs text-muted-foreground">
                ({actionLogData?.visible_count || 0})
              </span>
            </div>
            {guiPanelCollapsed ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronUp className="w-4 h-4 text-muted-foreground" />
            )}
          </button>
          {!guiPanelCollapsed && (
            <div className="border-t border-border/50 max-h-64 overflow-auto">
              {actionLogData && actionLogData.actions.length > 0 ? (
                <ActionLogTable
                  actions={actionLogData.actions}
                  onRowClick={onActionRowClick}
                  workflowStartTime={actionLogData.workflow_start_time ?? undefined}
                />
              ) : (
                <div className="p-4 text-center text-muted-foreground text-sm">
                  No actions recorded yet
                </div>
              )}
            </div>
          )}
        </div>

        {/* Image Recognition Panel */}
        <div className="card">
          <button
            onClick={() => setAiPanelCollapsed(!aiPanelCollapsed)}
            className="w-full flex items-center justify-between p-3 hover:bg-muted/30 transition-colors"
          >
            <div className="flex items-center gap-2">
              <Image className="w-4 h-4 text-purple-500" />
              <h2 className="font-medium text-sm">Image Recognition</h2>
              <span className="text-xs text-muted-foreground">({imageLogs.length})</span>
            </div>
            {aiPanelCollapsed ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronUp className="w-4 h-4 text-muted-foreground" />
            )}
          </button>
          {!aiPanelCollapsed && (
            <div className="border-t border-border/50 max-h-64 overflow-auto">
              {imageLogs.length > 0 ? (
                <ImageLogTable imageLogs={imageLogs} onRowClick={onImageRowClick} />
              ) : (
                <div className="p-4 text-center text-muted-foreground text-sm">
                  No image recognition results yet
                </div>
              )}
            </div>
          )}
        </div>
      </div>

      {/* AI Output - extends to bottom */}
      <div className="flex-1 flex flex-col min-h-0 card overflow-hidden">
        <div className="flex-shrink-0 flex items-center justify-between p-3 border-b border-border/50">
          <div className="flex items-center gap-2">
            <Bot className="w-4 h-4 text-purple-500" />
            <h2 className="font-medium text-sm">AI Output</h2>
            {isAiRunning && <Sparkles className="w-4 h-4 animate-pulse text-purple-500" />}
          </div>
          {activeSessions.length > 0 && (
            <span className="text-xs text-muted-foreground">
              {activeSessions.length} active session{activeSessions.length !== 1 ? "s" : ""}
            </span>
          )}
        </div>
        <div className="flex-1 min-h-0 overflow-hidden">
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
