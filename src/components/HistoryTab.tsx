/**
 * HistoryTab.tsx
 *
 * Unified history view showing both GUI automation runs and AI task runs.
 * Provides a comprehensive view of all automation activity with filtering
 * and search capabilities.
 *
 * Features:
 * - Combined timeline of GUI and AI runs
 * - Filter by type (GUI/AI/All)
 * - Filter by status (Success/Failed/All)
 * - Search by name
 * - Click to view run details
 */

import { useState, useEffect, useMemo, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  History,
  CheckCircle,
  XCircle,
  Clock,
  AlertTriangle,
  Monitor,
  Bot,
  Search,
  Filter,
  ChevronRight,
  RefreshCw,
  Trash2,
  Calendar,
} from "lucide-react";
import type { RunDetails, TieredInfoResponse } from "../types/statistics";

// Storage key for filter preferences
const STORAGE_KEY_FILTER = "historyTab.filter";

type FilterType = "all" | "gui" | "ai";
type FilterStatus = "all" | "success" | "failed";

interface AiSessionRun {
  id: string;
  name: string;
  startedAt: string;
  endedAt?: string;
  status: "completed" | "failed" | "running" | "cancelled";
  loopCount: number;
  taskType?: string;
}

interface UnifiedRun {
  id: string;
  type: "gui" | "ai";
  name: string;
  startedAt: string;
  endedAt?: string;
  durationMs?: number;
  status: string;
  success?: boolean;
  error?: string;
  // GUI-specific
  workflowName?: string;
  statesVisited?: string[];
  // AI-specific
  loopCount?: number;
  taskType?: string;
}

interface HistoryTabProps {
  onNavigateToRun: () => void;
  onNavigateToAi: () => void;
}

export function HistoryTab({ onNavigateToRun, onNavigateToAi }: HistoryTabProps) {
  // Filter state
  const [filterType, setFilterType] = useState<FilterType>(() => {
    try {
      const stored = localStorage.getItem(STORAGE_KEY_FILTER);
      if (stored) {
        const parsed = JSON.parse(stored);
        return parsed.type || "all";
      }
    } catch {
      // ignore
    }
    return "all";
  });
  const [filterStatus, setFilterStatus] = useState<FilterStatus>(() => {
    try {
      const stored = localStorage.getItem(STORAGE_KEY_FILTER);
      if (stored) {
        const parsed = JSON.parse(stored);
        return parsed.status || "all";
      }
    } catch {
      // ignore
    }
    return "all";
  });
  const [searchQuery, setSearchQuery] = useState("");

  // Data state
  const [guiRuns, setGuiRuns] = useState<RunDetails[]>([]);
  const [aiRuns, setAiRuns] = useState<AiSessionRun[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Selection for deletion
  const [selectedRuns, setSelectedRuns] = useState<Set<string>>(new Set());
  const [isDeleting, setIsDeleting] = useState(false);

  // Persist filter preferences
  useEffect(() => {
    localStorage.setItem(
      STORAGE_KEY_FILTER,
      JSON.stringify({ type: filterType, status: filterStatus }),
    );
  }, [filterType, filterStatus]);

  // Fetch GUI runs
  const fetchGuiRuns = useCallback(async () => {
    try {
      const result = await invoke<TieredInfoResponse<RunDetails[]>>("get_recent_runs", {
        limit: 100,
      });
      if (result.success && result.data) {
        setGuiRuns(result.data);
      }
    } catch (err) {
      console.error("Failed to fetch GUI runs:", err);
    }
  }, []);

  // Fetch AI runs
  const fetchAiRuns = useCallback(async () => {
    try {
      const result = await invoke<{
        success: boolean;
        data?: AiSessionRun[];
        error?: string;
      }>("get_ai_session_history", { limit: 100 });
      if (result.success && result.data) {
        setAiRuns(result.data);
      }
    } catch (err) {
      // Silently ignore - endpoint may not exist yet
      console.debug("AI session history not available:", err);
    }
  }, []);

  // Initial fetch
  useEffect(() => {
    const fetchAll = async () => {
      setLoading(true);
      setError(null);
      try {
        await Promise.all([fetchGuiRuns(), fetchAiRuns()]);
      } catch (err) {
        setError(String(err));
      } finally {
        setLoading(false);
      }
    };
    fetchAll();
  }, [fetchGuiRuns, fetchAiRuns]);

  // Combine and sort runs
  const unifiedRuns = useMemo<UnifiedRun[]>(() => {
    const combined: UnifiedRun[] = [];

    // Add GUI runs
    for (const run of guiRuns) {
      combined.push({
        id: run.id,
        type: "gui",
        name: run.workflow_name || "GUI Workflow",
        startedAt: run.started_at,
        endedAt: run.ended_at,
        durationMs: run.duration_ms,
        status: run.status,
        success: run.success,
        error: run.error_message,
        workflowName: run.workflow_name,
        statesVisited: run.states_visited,
      });
    }

    // Add AI runs
    for (const run of aiRuns) {
      combined.push({
        id: run.id,
        type: "ai",
        name: run.name || "AI Task",
        startedAt: run.startedAt,
        endedAt: run.endedAt,
        durationMs: run.endedAt
          ? new Date(run.endedAt).getTime() - new Date(run.startedAt).getTime()
          : undefined,
        status: run.status,
        success: run.status === "completed",
        loopCount: run.loopCount,
        taskType: run.taskType,
      });
    }

    // Sort by startedAt descending
    combined.sort((a, b) => new Date(b.startedAt).getTime() - new Date(a.startedAt).getTime());

    return combined;
  }, [guiRuns, aiRuns]);

  // Apply filters
  const filteredRuns = useMemo(() => {
    return unifiedRuns.filter((run) => {
      // Type filter
      if (filterType !== "all" && run.type !== filterType) {
        return false;
      }

      // Status filter
      if (filterStatus === "success" && run.success !== true) {
        return false;
      }
      if (filterStatus === "failed" && run.success !== false) {
        return false;
      }

      // Search filter
      if (searchQuery) {
        const query = searchQuery.toLowerCase();
        const nameMatch = run.name.toLowerCase().includes(query);
        const workflowMatch = run.workflowName?.toLowerCase().includes(query);
        if (!nameMatch && !workflowMatch) {
          return false;
        }
      }

      return true;
    });
  }, [unifiedRuns, filterType, filterStatus, searchQuery]);

  // Format helpers
  const formatDuration = (ms: number | undefined) => {
    if (!ms) return "-";
    if (ms < 1000) return `${ms}ms`;
    if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
    return `${Math.floor(ms / 60000)}m ${Math.floor((ms % 60000) / 1000)}s`;
  };

  const formatTime = (iso: string) => {
    const date = new Date(iso);
    const now = new Date();
    const isToday = date.toDateString() === now.toDateString();
    const isYesterday = new Date(now.getTime() - 86400000).toDateString() === date.toDateString();

    if (isToday) {
      return `Today ${date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;
    }
    if (isYesterday) {
      return `Yesterday ${date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;
    }
    return date.toLocaleString([], {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  };

  // Handle refresh
  const handleRefresh = async () => {
    setLoading(true);
    await Promise.all([fetchGuiRuns(), fetchAiRuns()]);
    setLoading(false);
  };

  // Handle delete selected
  const handleDeleteSelected = async () => {
    if (selectedRuns.size === 0) return;

    setIsDeleting(true);
    try {
      for (const id of selectedRuns) {
        const run = unifiedRuns.find((r) => r.id === id);
        if (run?.type === "gui") {
          await invoke("delete_run", { runId: id });
        } else {
          await invoke("delete_ai_session", { sessionId: id });
        }
      }
      setSelectedRuns(new Set());
      await handleRefresh();
    } catch (err) {
      console.error("Failed to delete runs:", err);
    } finally {
      setIsDeleting(false);
    }
  };

  // Toggle selection
  const toggleSelection = (id: string) => {
    const newSelected = new Set(selectedRuns);
    if (newSelected.has(id)) {
      newSelected.delete(id);
    } else {
      newSelected.add(id);
    }
    setSelectedRuns(newSelected);
  };

  // Stats
  const stats = useMemo(() => {
    const total = filteredRuns.length;
    const success = filteredRuns.filter((r) => r.success).length;
    const failed = filteredRuns.filter((r) => r.success === false).length;
    const guiCount = filteredRuns.filter((r) => r.type === "gui").length;
    const aiCount = filteredRuns.filter((r) => r.type === "ai").length;
    return { total, success, failed, guiCount, aiCount };
  }, [filteredRuns]);

  return (
    <div className="h-full flex flex-col p-4 space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <History className="w-5 h-5 text-primary" />
          <h1 className="text-xl font-semibold">Run History</h1>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={handleRefresh}
            disabled={loading}
            className="p-2 rounded-lg hover:bg-muted transition-colors"
            title="Refresh"
          >
            <RefreshCw className={`w-4 h-4 ${loading ? "animate-spin" : ""}`} />
          </button>
          {selectedRuns.size > 0 && (
            <button
              onClick={handleDeleteSelected}
              disabled={isDeleting}
              className="flex items-center gap-2 px-3 py-1.5 text-sm bg-red-500/10 text-red-500 rounded-lg hover:bg-red-500/20 transition-colors"
            >
              <Trash2 className="w-4 h-4" />
              Delete {selectedRuns.size}
            </button>
          )}
        </div>
      </div>

      {/* Filters */}
      <div className="flex items-center gap-4 flex-wrap">
        {/* Type filter */}
        <div className="flex items-center gap-2">
          <Filter className="w-4 h-4 text-muted-foreground" />
          <div className="flex rounded-lg border border-border overflow-hidden">
            {(["all", "gui", "ai"] as FilterType[]).map((type) => (
              <button
                key={type}
                onClick={() => setFilterType(type)}
                className={`px-3 py-1.5 text-sm ${
                  filterType === type ? "bg-primary text-primary-foreground" : "hover:bg-muted"
                }`}
              >
                {type === "all" && "All"}
                {type === "gui" && (
                  <span className="flex items-center gap-1">
                    <Monitor className="w-3 h-3" /> GUI
                  </span>
                )}
                {type === "ai" && (
                  <span className="flex items-center gap-1">
                    <Bot className="w-3 h-3" /> AI
                  </span>
                )}
              </button>
            ))}
          </div>
        </div>

        {/* Status filter */}
        <div className="flex rounded-lg border border-border overflow-hidden">
          {(["all", "success", "failed"] as FilterStatus[]).map((status) => (
            <button
              key={status}
              onClick={() => setFilterStatus(status)}
              className={`px-3 py-1.5 text-sm ${
                filterStatus === status ? "bg-primary text-primary-foreground" : "hover:bg-muted"
              }`}
            >
              {status === "all" && "All"}
              {status === "success" && (
                <span className="flex items-center gap-1">
                  <CheckCircle className="w-3 h-3" /> Passed
                </span>
              )}
              {status === "failed" && (
                <span className="flex items-center gap-1">
                  <XCircle className="w-3 h-3" /> Failed
                </span>
              )}
            </button>
          ))}
        </div>

        {/* Search */}
        <div className="flex-1 max-w-xs">
          <div className="relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
            <input
              type="text"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder="Search runs..."
              className="w-full pl-9 pr-4 py-1.5 text-sm border border-border rounded-lg bg-background focus:outline-none focus:ring-2 focus:ring-primary"
            />
          </div>
        </div>
      </div>

      {/* Stats bar */}
      <div className="flex items-center gap-6 text-sm text-muted-foreground">
        <span>{stats.total} runs</span>
        <span className="text-green-500">{stats.success} passed</span>
        <span className="text-red-500">{stats.failed} failed</span>
        <span className="flex items-center gap-1">
          <Monitor className="w-3 h-3" /> {stats.guiCount}
        </span>
        <span className="flex items-center gap-1">
          <Bot className="w-3 h-3" /> {stats.aiCount}
        </span>
      </div>

      {/* Run list */}
      <div className="flex-1 overflow-y-auto">
        {loading && filteredRuns.length === 0 ? (
          <div className="text-center py-8 text-muted-foreground">Loading...</div>
        ) : error ? (
          <div className="text-center py-8 text-red-500">{error}</div>
        ) : filteredRuns.length === 0 ? (
          <div className="text-center py-8 text-muted-foreground">
            <History className="w-12 h-12 mx-auto mb-4 opacity-50" />
            <p>No runs found</p>
            <p className="text-sm mt-2">
              {filterType !== "all" || filterStatus !== "all" || searchQuery
                ? "Try adjusting your filters"
                : "Start an automation to see history"}
            </p>
          </div>
        ) : (
          <div className="space-y-2">
            {filteredRuns.map((run) => (
              <div
                key={run.id}
                className={`card p-3 flex items-center gap-3 hover:bg-muted/30 transition-colors cursor-pointer ${
                  selectedRuns.has(run.id) ? "ring-2 ring-primary" : ""
                }`}
                onClick={() => toggleSelection(run.id)}
              >
                {/* Type indicator */}
                <div
                  className={`p-2 rounded-lg ${
                    run.type === "gui" ? "bg-blue-500/10" : "bg-purple-500/10"
                  }`}
                >
                  {run.type === "gui" ? (
                    <Monitor className="w-4 h-4 text-blue-500" />
                  ) : (
                    <Bot className="w-4 h-4 text-purple-500" />
                  )}
                </div>

                {/* Status icon */}
                {run.success === true ? (
                  <CheckCircle className="w-4 h-4 text-green-500 flex-shrink-0" />
                ) : run.success === false ? (
                  <XCircle className="w-4 h-4 text-red-500 flex-shrink-0" />
                ) : run.status === "running" ? (
                  <Clock className="w-4 h-4 text-blue-500 animate-pulse flex-shrink-0" />
                ) : (
                  <AlertTriangle className="w-4 h-4 text-yellow-500 flex-shrink-0" />
                )}

                {/* Run info */}
                <div className="flex-1 min-w-0">
                  <div className="font-medium truncate">{run.name}</div>
                  <div className="text-xs text-muted-foreground flex items-center gap-2">
                    <Calendar className="w-3 h-3" />
                    {formatTime(run.startedAt)}
                    {run.durationMs && (
                      <>
                        <span className="text-muted-foreground/50">|</span>
                        <Clock className="w-3 h-3" />
                        {formatDuration(run.durationMs)}
                      </>
                    )}
                    {run.loopCount !== undefined && run.loopCount > 0 && (
                      <>
                        <span className="text-muted-foreground/50">|</span>
                        {run.loopCount} loops
                      </>
                    )}
                  </div>
                </div>

                {/* Error preview */}
                {run.error && (
                  <div className="text-xs text-red-500 max-w-[200px] truncate">{run.error}</div>
                )}

                {/* Actions */}
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    if (run.type === "gui") {
                      onNavigateToRun();
                    } else {
                      onNavigateToAi();
                    }
                  }}
                  className="p-1 rounded hover:bg-muted"
                >
                  <ChevronRight className="w-4 h-4 text-muted-foreground" />
                </button>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

export default HistoryTab;
