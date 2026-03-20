/**
 * WorkflowQueueTab
 *
 * Main page component for selecting and executing unified workflows as a sequence.
 * Users can browse the workflow library, add workflows to a queue, reorder them,
 * and execute the entire sequence.
 */

import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import { instanceStorage } from "@/lib/instance-storage";
import { Play, RefreshCw, Trash2, Loader2, Settings2, ChevronRight } from "lucide-react";
import { WorkflowLibraryPanel } from "./WorkflowLibraryPanel";
import { WorkflowQueuePanel } from "./WorkflowQueuePanel";
import { SlashCommandArgsDialog } from "./SlashCommandArgsDialog";
import type { WorkflowWithStats, QueuedWorkflow, WorkflowStats } from "./types";
import type { UnifiedWorkflow } from "../../types/unified-workflow";
import { getTotalStepCount } from "../../types/unified-workflow";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

const STORAGE_KEY = "qontinui-workflow-queue";
const OPTIONS_STORAGE_KEY = "qontinui-workflow-queue-options";
const LIBRARY_OPEN_KEY = "qontinui-workflow-queue-library-open";

type LogLevel = "info" | "warning" | "error" | "debug" | "success";

interface WorkflowQueueTabProps {
  onNavigateToActive: () => void;
  onLog?: (level: LogLevel, message: string) => void;
}

export function WorkflowQueueTab({ onNavigateToActive, onLog }: WorkflowQueueTabProps) {
  // Library state
  const [workflows, setWorkflows] = useState<WorkflowWithStats[]>([]);
  const [loading, setLoading] = useState(true);
  const [search, setSearch] = useState("");
  const [category, setCategory] = useState<string>("all");

  // Queue state - persisted to instanceStorage
  const [queue, setQueue] = useState<QueuedWorkflow[]>(() => {
    return instanceStorage.getJSON<QueuedWorkflow[]>(STORAGE_KEY, []);
  });

  // Execution options
  const [stopOnFailure, setStopOnFailure] = useState<boolean>(() => {
    const parsed = instanceStorage.getJSON<{ stopOnFailure?: boolean } | null>(
      OPTIONS_STORAGE_KEY,
      null,
    );
    return parsed?.stopOnFailure ?? true;
  });
  const [showOptions, setShowOptions] = useState(false);

  // Library panel visibility -- persisted to instanceStorage
  const [isLibraryOpen, setIsLibraryOpen] = useState<boolean>(() => {
    return instanceStorage.getJSON(LIBRARY_OPEN_KEY, true);
  });

  const toggleLibrary = useCallback(() => {
    setIsLibraryOpen((prev) => {
      const next = !prev;
      instanceStorage.setJSON(LIBRARY_OPEN_KEY, next);
      return next;
    });
  }, []);

  // Favorites filter
  const [showFavoritesOnly, setShowFavoritesOnly] = useState(false);

  // Execution state
  const [isExecuting, setIsExecuting] = useState(false);

  // Slash command sync state
  const [isSyncing, setIsSyncing] = useState(false);

  // Args dialog state (for slash commands with $ARGUMENTS)
  const [argsDialogWorkflow, setArgsDialogWorkflow] = useState<WorkflowWithStats | null>(null);

  // Persist queue to instanceStorage
  useEffect(() => {
    instanceStorage.setJSON(STORAGE_KEY, queue);
  }, [queue]);

  // Persist options to instanceStorage
  useEffect(() => {
    instanceStorage.setJSON(OPTIONS_STORAGE_KEY, { stopOnFailure });
  }, [stopOnFailure]);

  // Fetch workflows with stats
  const fetchWorkflows = useCallback(async () => {
    setLoading(true);
    try {
      // Fetch workflows
      const workflowsResponse = await tracedFetch(`${getApiBase()}/unified-workflows`);
      const workflowsResult = await workflowsResponse.json();

      if (!workflowsResult.success) {
        throw new Error(workflowsResult.error || "Failed to fetch workflows");
      }

      const rawWorkflows: UnifiedWorkflow[] = workflowsResult.data || [];

      // Fetch stats for each workflow
      const workflowsWithStats: WorkflowWithStats[] = await Promise.all(
        rawWorkflows.map(async (workflow) => {
          try {
            const statsResponse = await tracedFetch(
              `${getApiBase()}/unified-workflows/${workflow.id}/stats`,
            );
            const statsResult = await statsResponse.json();

            if (statsResult.success && statsResult.data) {
              return {
                ...workflow,
                stats: statsResult.data as WorkflowStats,
              };
            }
          } catch {
            // Ignore stats fetch errors
          }

          // Return with empty stats if fetch fails
          return {
            ...workflow,
            stats: {
              totalRuns: 0,
              successCount: 0,
              failureCount: 0,
              lastRunAt: null,
              lastRunStatus: null,
              avgDurationMs: null,
            },
          };
        }),
      );

      setWorkflows(workflowsWithStats);

      // Update queue items with fresh data
      setQueue((prevQueue) =>
        prevQueue.map((item) => {
          const freshWorkflow = workflowsWithStats.find((w) => w.id === item.workflow.id);
          if (freshWorkflow) {
            return { ...item, workflow: freshWorkflow };
          }
          return item;
        }),
      );
    } catch (error) {
      console.error("Failed to fetch workflows:", error);
      onLog?.("error", `Failed to fetch workflows: ${error}`);
    } finally {
      setLoading(false);
    }
  }, [onLog]);

  useEffect(() => {
    fetchWorkflows();
  }, [fetchWorkflows]);

  // Toggle favorite on a workflow (optimistic update with rollback)
  const toggleFavorite = useCallback(async (workflowId: string) => {
    setWorkflows((prev) =>
      prev.map((w) => (w.id === workflowId ? { ...w, is_favorite: !w.is_favorite } : w)),
    );
    try {
      const response = await tracedFetch(
        `${getApiBase()}/unified-workflows/${workflowId}/favorite`,
        { method: "POST" },
      );
      const result = await response.json();
      if (result.success) {
        const newState = result.data.is_favorite;
        setWorkflows((prev) =>
          prev.map((w) => (w.id === workflowId ? { ...w, is_favorite: newState } : w)),
        );
      } else {
        setWorkflows((prev) =>
          prev.map((w) => (w.id === workflowId ? { ...w, is_favorite: !w.is_favorite } : w)),
        );
      }
    } catch (error) {
      console.error("Failed to toggle favorite:", error);
      setWorkflows((prev) =>
        prev.map((w) => (w.id === workflowId ? { ...w, is_favorite: !w.is_favorite } : w)),
      );
    }
  }, []);

  // Check if any workflows are favorited
  const hasFavorites = useMemo(() => workflows.some((w) => w.is_favorite), [workflows]);

  // Auto-enable favorites filter when favorites exist on first load only
  const hasInitializedFavorites = useRef(false);
  useEffect(() => {
    if (!hasInitializedFavorites.current && hasFavorites && workflows.length > 0 && !loading) {
      hasInitializedFavorites.current = true;
      setShowFavoritesOnly(true);
    }
  }, [hasFavorites, workflows.length, loading]);

  // Queue operations
  const addToQueue = useCallback((workflow: WorkflowWithStats) => {
    setQueue((prev) => [
      ...prev,
      {
        queueId: crypto.randomUUID(),
        workflow,
        position: prev.length,
      },
    ]);
  }, []);

  const removeFromQueue = useCallback((queueId: string) => {
    setQueue((prev) =>
      prev
        .filter((item) => item.queueId !== queueId)
        .map((item, index) => ({ ...item, position: index })),
    );
  }, []);

  const reorderQueue = useCallback((fromIndex: number, toIndex: number) => {
    setQueue((prev) => {
      const newQueue = [...prev];
      const [moved] = newQueue.splice(fromIndex, 1);
      newQueue.splice(toIndex, 0, moved);
      return newQueue.map((item, index) => ({ ...item, position: index }));
    });
  }, []);

  const clearQueue = useCallback(() => {
    setQueue([]);
  }, []);

  // Sync slash commands from disk
  const syncCommands = useCallback(async () => {
    setIsSyncing(true);
    try {
      const response = await tracedFetch(`${getApiBase()}/slash-commands/sync`, {
        method: "POST",
      });
      const result = await response.json();
      if (result.success) {
        const data = result.data;
        onLog?.(
          "success",
          `Commands synced: ${data.created} new, ${data.updated} updated, ${data.deleted} removed`,
        );
        // Refresh the workflow list
        await fetchWorkflows();
      } else {
        throw new Error(result.error || "Sync failed");
      }
    } catch (error) {
      onLog?.("error", `Failed to sync commands: ${error}`);
    } finally {
      setIsSyncing(false);
    }
  }, [onLog, fetchWorkflows]);

  // Run a single workflow directly (used for slash commands with args)
  const runSingleWorkflow = useCallback(
    async (workflowId: string, args?: string) => {
      try {
        const body: Record<string, unknown> = {};
        if (args !== undefined) {
          body.arguments = args;
        }
        const response = await tracedFetch(`${getApiBase()}/unified-workflows/${workflowId}/run`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(body),
        });
        const result = await response.json();
        if (result.success) {
          onLog?.("success", "Started workflow execution");
          onNavigateToActive();
        } else {
          throw new Error(result.error || "Failed to start workflow");
        }
      } catch (error) {
        onLog?.("error", `Failed to run workflow: ${error}`);
      }
    },
    [onLog, onNavigateToActive],
  );

  // Handle adding a workflow to queue — intercept for slash commands with args
  const handleAddToQueue = useCallback(
    (workflow: WorkflowWithStats) => {
      if (workflow.tags?.includes("requires-arguments")) {
        setArgsDialogWorkflow(workflow);
      } else {
        addToQueue(workflow);
      }
    },
    [addToQueue],
  );

  // Execute queue
  const executeQueue = useCallback(async () => {
    if (queue.length === 0) return;

    setIsExecuting(true);
    try {
      const response = await tracedFetch(`${getApiBase()}/unified-workflows/run-composed`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          workflow_ids: queue.map((item) => item.workflow.id),
          stop_on_failure: stopOnFailure,
        }),
      });

      const result = await response.json();
      if (result.success) {
        onLog?.("success", `Started composed workflow: ${queue.length} stages`);
        onNavigateToActive();
      } else {
        throw new Error(result.error || "Failed to start composed workflow");
      }
    } catch (error) {
      onLog?.("error", `Failed to execute queue: ${error}`);
    } finally {
      setIsExecuting(false);
    }
  }, [queue, stopOnFailure, onLog, onNavigateToActive]);

  // Filter workflows
  const filteredWorkflows = useMemo(
    () =>
      workflows.filter((w) => {
        const matchesSearch =
          search === "" ||
          w.name.toLowerCase().includes(search.toLowerCase()) ||
          w.description.toLowerCase().includes(search.toLowerCase());
        const matchesCategory = category === "all" || w.category === category;
        const matchesFavorites = !showFavoritesOnly || w.is_favorite;
        return matchesSearch && matchesCategory && matchesFavorites;
      }),
    [workflows, search, category, showFavoritesOnly],
  );

  // Get unique categories
  const categories = useMemo(
    () => ["all", ...new Set(workflows.map((w) => w.category).filter(Boolean))],
    [workflows],
  );

  // Calculate totals
  const totalSteps = queue.reduce((sum, item) => sum + getTotalStepCount(item.workflow), 0);

  // Get queued workflow IDs (for showing "already in queue" state)
  const queuedIds = useMemo(() => new Set(queue.map((q) => q.workflow.id)), [queue]);

  return (
    <div className="h-full flex flex-col bg-background">
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 border-b border-border">
        <h1 className="text-h2 text-foreground">Workflow Queue</h1>
        <button
          onClick={fetchWorkflows}
          disabled={loading}
          className="btn-secondary flex items-center gap-2"
        >
          <RefreshCw className={`w-4 h-4 ${loading ? "animate-spin" : ""}`} />
          Refresh
        </button>
      </div>

      {/* Main content - two panels */}
      <div className="flex-1 flex overflow-hidden">
        {/* Left: Library (collapsible) */}
        {isLibraryOpen ? (
          <div className="flex-shrink-0 flex" style={{ width: 280 }}>
            <WorkflowLibraryPanel
              workflows={filteredWorkflows}
              loading={loading}
              search={search}
              onSearchChange={setSearch}
              category={category}
              onCategoryChange={setCategory}
              categories={categories}
              onAddToQueue={handleAddToQueue}
              queuedIds={queuedIds}
              onCollapse={toggleLibrary}
              showFavoritesOnly={showFavoritesOnly}
              onToggleFavoritesFilter={() => setShowFavoritesOnly((prev) => !prev)}
              onToggleFavorite={toggleFavorite}
              hasFavorites={hasFavorites}
              onSyncCommands={syncCommands}
              isSyncing={isSyncing}
            />
          </div>
        ) : (
          /* Collapsed library strip */
          <button
            onClick={toggleLibrary}
            className="flex-shrink-0 flex flex-col items-center justify-center gap-2 w-8 border-r border-white/10
                       text-muted-foreground hover:text-foreground hover:bg-white/5 transition-colors"
            title="Expand workflow library"
          >
            <ChevronRight className="w-4 h-4" />
            <span
              className="text-xs font-medium"
              style={{ writingMode: "vertical-rl", transform: "rotate(180deg)" }}
            >
              Library
            </span>
          </button>
        )}

        {/* Right: Queue */}
        <WorkflowQueuePanel items={queue} onRemove={removeFromQueue} onReorder={reorderQueue} />
      </div>

      {/* Bottom bar */}
      <div className="flex items-center justify-between px-4 py-3 border-t border-border bg-card/50">
        <div className="text-body-sm text-muted-foreground">
          Queue: {queue.length} workflow{queue.length !== 1 ? "s" : ""}
          {queue.length > 0 && ` • ${totalSteps} total steps`}
        </div>
        <div className="flex items-center gap-2">
          {/* Options toggle */}
          <div className="relative">
            <button
              onClick={() => setShowOptions(!showOptions)}
              className={`p-2 rounded-lg transition-colors ${
                showOptions
                  ? "bg-primary/20 text-primary"
                  : "text-muted-foreground hover:text-foreground hover:bg-white/5"
              }`}
              title="Execution options"
            >
              <Settings2 className="w-4 h-4" />
            </button>

            {/* Options dropdown */}
            {showOptions && (
              <div className="absolute bottom-full right-0 mb-2 w-64 card p-3 shadow-xl z-20">
                <h4 className="text-label text-foreground mb-2">Execution Options</h4>
                <label className="flex items-center gap-2 text-body-sm text-muted-foreground cursor-pointer">
                  <input
                    type="checkbox"
                    checked={stopOnFailure}
                    onChange={(e) => setStopOnFailure(e.target.checked)}
                    className="w-4 h-4 rounded border-border bg-muted text-primary focus:ring-primary"
                  />
                  Stop on failure
                </label>
                <p className="text-label-sm text-muted-foreground mt-1 ml-6">
                  Stop the sequence if any workflow fails
                </p>
              </div>
            )}
          </div>

          <button
            onClick={clearQueue}
            disabled={queue.length === 0 || isExecuting}
            className="btn-secondary flex items-center gap-2"
          >
            <Trash2 className="w-4 h-4" />
            Clear
          </button>
          <button
            onClick={executeQueue}
            disabled={queue.length === 0 || isExecuting}
            className="btn-primary flex items-center gap-2"
          >
            {isExecuting ? (
              <>
                <Loader2 className="w-4 h-4 animate-spin" />
                Starting...
              </>
            ) : (
              <>
                <Play className="w-4 h-4" />
                Run Queue
              </>
            )}
          </button>
        </div>
      </div>

      {/* Click outside to close options */}
      {showOptions && <div className="fixed inset-0 z-10" onClick={() => setShowOptions(false)} />}

      {/* Slash command args dialog */}
      {argsDialogWorkflow && (
        <SlashCommandArgsDialog
          workflowName={argsDialogWorkflow.name}
          onRun={(args) => {
            runSingleWorkflow(argsDialogWorkflow.id, args);
            setArgsDialogWorkflow(null);
          }}
          onCancel={() => setArgsDialogWorkflow(null)}
        />
      )}
    </div>
  );
}
