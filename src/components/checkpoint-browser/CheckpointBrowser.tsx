/**
 * CheckpointBrowser.tsx
 *
 * Main Checkpoint Browser page for time-travel debugging.
 * Shows timeline of checkpoints, detail view, and comparison mode.
 */

import { useState } from "react";
import {
  Clock,
  RefreshCw,
  AlertTriangle,
  GitCompare,
  GitBranch,
  Play,
  Trash2,
  FlaskConical,
  X,
  ChevronDown,
  Radio,
} from "lucide-react";
import {
  useCheckpointList,
  useCheckpoint,
  useCheckpointDiff,
  useCheckpointTaskIds,
  useCheckpointStats,
  useLineageTree,
  useAutoSelectTask,
  checkpointActions,
} from "./useCheckpointData";
import { CheckpointTimeline } from "./CheckpointTimeline";
import { StateSnapshotView } from "./StateSnapshotView";
import { CheckpointDiffView } from "./CheckpointDiffView";
import { LineageTreeView } from "./LineageTreeView";

export function CheckpointBrowser() {
  const { taskIds, isLoading: taskIdsLoading } = useCheckpointTaskIds();
  // Use auto-select hook to automatically select the most recent task with checkpoints
  const { selectedTaskId, setSelectedTaskId, isAutoSelected } = useAutoSelectTask(taskIds);

  const [selectedCheckpointId, setSelectedCheckpointId] = useState<string | null>(null);
  const [compareCheckpointId, setCompareCheckpointId] = useState<string | null>(null);
  const [compareMode, setCompareMode] = useState(false);
  const [showLineage, setShowLineage] = useState(false);
  const [lineageExpanded, setLineageExpanded] = useState(false);
  const [actionInProgress, setActionInProgress] = useState(false);
  const { checkpoints, isLoading, error, refetch, isRealtimeConnected, activeCheckpointId } =
    useCheckpointList(selectedTaskId);
  const { checkpoint: selectedCheckpoint, isLoading: loadingCheckpoint } =
    useCheckpoint(selectedCheckpointId);
  const { diff, isLoading: loadingDiff } = useCheckpointDiff(
    compareMode && selectedCheckpointId ? selectedCheckpointId : null,
    compareMode && compareCheckpointId ? compareCheckpointId : null,
  );
  const { stats, refetch: refetchStats } = useCheckpointStats();

  // Get lineage for the selected task (from selected checkpoint or task filter)
  const lineageTaskId = selectedCheckpoint?.task_id || selectedTaskId;
  const { lineage, isLoading: loadingLineage } = useLineageTree(
    showLineage && lineageTaskId ? lineageTaskId : null,
  );

  // Check if task has lineage (replayed task or has replay children)
  const hasLineage =
    lineage &&
    lineage.nodes.length > 0 &&
    (lineage.nodes.length > 1 || lineage.nodes.some((n) => n.checkpoint_id !== null));

  const handleSelectCheckpoint = (id: string) => {
    setSelectedCheckpointId(id);
    if (!compareMode) {
      setCompareCheckpointId(null);
    }
  };

  const handleCompareCheckpoint = (id: string) => {
    if (id === selectedCheckpointId) return;
    setCompareCheckpointId(id);
  };

  const toggleCompareMode = () => {
    setCompareMode(!compareMode);
    if (compareMode) {
      setCompareCheckpointId(null);
    }
  };

  const toggleLineageView = () => {
    setShowLineage(!showLineage);
    if (showLineage) {
      setLineageExpanded(false);
    }
  };

  const handleNavigateToTask = (taskId: string) => {
    // Set the task filter to the clicked task
    setSelectedTaskId(taskId);
    setSelectedCheckpointId(null);
    setCompareCheckpointId(null);
    // Close expanded lineage view
    setLineageExpanded(false);
  };

  const handleAddSampleData = async () => {
    setActionInProgress(true);
    try {
      await checkpointActions.addSampleCheckpoints();
      await refetch();
      await refetchStats();
    } finally {
      setActionInProgress(false);
    }
  };

  const handleClearData = async () => {
    if (!confirm("Are you sure you want to clear all checkpoints?")) return;
    setActionInProgress(true);
    try {
      await checkpointActions.clearAllCheckpoints();
      setSelectedCheckpointId(null);
      setCompareCheckpointId(null);
      await refetch();
      await refetchStats();
    } finally {
      setActionInProgress(false);
    }
  };

  const [replayStatus, setReplayStatus] = useState<{
    loading: boolean;
    success: boolean | null;
    message: string | null;
  }>({ loading: false, success: null, message: null });

  const handleStartReplay = async () => {
    if (!selectedCheckpointId) return;

    setReplayStatus({ loading: true, success: null, message: null });

    try {
      const response = await checkpointActions.replayFromCheckpoint(selectedCheckpointId);

      setReplayStatus({
        loading: false,
        success: true,
        message: `Replay started! New task: ${response.new_task_run_id}\nRestoring to state: ${response.restoration_instructions.target_state} (iteration ${response.restoration_instructions.resume_iteration})`,
      });

      // Clear the message after a delay
      setTimeout(() => {
        setReplayStatus((prev) => ({ ...prev, message: null }));
      }, 5000);
    } catch (err) {
      console.error("Failed to start replay:", err);
      setReplayStatus({
        loading: false,
        success: false,
        message: err instanceof Error ? err.message : "Failed to start replay session",
      });
    }
  };

  if (isLoading && checkpoints.length === 0) {
    return (
      <div className="h-full flex items-center justify-center">
        <RefreshCw className="w-6 h-6 animate-spin text-gray-400" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="h-full flex items-center justify-center">
        <div className="text-center">
          <AlertTriangle className="w-12 h-12 text-red-500 mx-auto mb-4" />
          <p className="text-gray-400">Failed to load checkpoints</p>
          <p className="text-sm text-gray-500 mt-1">{error}</p>
          <button
            onClick={refetch}
            className="mt-4 px-4 py-2 bg-gray-700 text-white rounded hover:bg-gray-600 transition-colors"
          >
            Retry
          </button>
        </div>
      </div>
    );
  }

  const isEmpty = checkpoints.length === 0 && !selectedTaskId;

  return (
    <div className="h-full flex flex-col">
      {/* Header */}
      <div className="flex items-center justify-between p-4 border-b border-gray-700">
        <div className="flex items-center gap-4">
          <div className="flex items-center gap-2">
            <Clock className="w-6 h-6 text-cyan-500" />
            <h1 className="text-xl font-bold text-white">Checkpoint Browser</h1>
            {/* Live indicator */}
            {isRealtimeConnected && (
              <div className="flex items-center gap-1 ml-2">
                <Radio
                  className={`w-3 h-3 ${activeCheckpointId ? "text-green-400 animate-pulse" : "text-green-600"}`}
                />
                <span
                  className={`text-xs ${activeCheckpointId ? "text-green-400" : "text-green-600"}`}
                >
                  {activeCheckpointId ? "LIVE" : "Connected"}
                </span>
              </div>
            )}
          </div>

          {/* Task Filter */}
          <div className="relative">
            <select
              value={selectedTaskId || ""}
              onChange={(e) => {
                setSelectedTaskId(e.target.value || undefined);
                setSelectedCheckpointId(null);
                setCompareCheckpointId(null);
              }}
              className={`appearance-none bg-gray-800 border rounded px-3 py-1.5 pr-8 text-sm text-white focus:outline-none focus:ring-2 focus:ring-cyan-500 ${
                isAutoSelected ? "border-cyan-500" : "border-gray-700"
              }`}
              title={isAutoSelected ? "Auto-selected most recent task" : undefined}
            >
              <option value="">All Tasks</option>
              {taskIds.map((tid) => (
                <option key={tid} value={tid}>
                  {tid}
                </option>
              ))}
            </select>
            <ChevronDown className="absolute right-2 top-1/2 -translate-y-1/2 w-4 h-4 text-gray-400 pointer-events-none" />
            {isAutoSelected && (
              <span className="absolute -top-2 -right-2 bg-cyan-500 text-xs text-white px-1.5 py-0.5 rounded-full">
                recent
              </span>
            )}
          </div>

          {/* Stats */}
          {stats && (
            <span className="text-xs text-gray-400">
              {stats.total_checkpoints} checkpoints across {stats.tasks_with_checkpoints} tasks
            </span>
          )}
        </div>

        <div className="flex items-center gap-2">
          {/* Compare Mode Toggle */}
          <button
            onClick={toggleCompareMode}
            className={`flex items-center gap-2 px-3 py-1.5 rounded transition-colors ${
              compareMode
                ? "bg-orange-600 text-white"
                : "bg-gray-700 text-gray-300 hover:bg-gray-600"
            }`}
          >
            <GitCompare className="w-4 h-4" />
            <span className="text-sm">Compare</span>
          </button>

          {/* View Lineage Toggle */}
          {(selectedTaskId || selectedCheckpoint) && (
            <button
              onClick={toggleLineageView}
              className={`flex items-center gap-2 px-3 py-1.5 rounded transition-colors ${
                showLineage
                  ? "bg-purple-600 text-white"
                  : "bg-gray-700 text-gray-300 hover:bg-gray-600"
              }`}
              title="View replay lineage tree"
            >
              <GitBranch className="w-4 h-4" />
              <span className="text-sm">Lineage</span>
            </button>
          )}

          {/* Replay Button */}
          {selectedCheckpointId && (
            <button
              onClick={handleStartReplay}
              disabled={replayStatus.loading}
              className="flex items-center gap-2 px-3 py-1.5 bg-green-600 text-white rounded hover:bg-green-500 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {replayStatus.loading ? (
                <RefreshCw className="w-4 h-4 animate-spin" />
              ) : (
                <Play className="w-4 h-4" />
              )}
              <span className="text-sm">{replayStatus.loading ? "Starting..." : "Replay"}</span>
            </button>
          )}

          <button
            onClick={handleAddSampleData}
            disabled={actionInProgress}
            className="p-2 text-gray-400 hover:text-white hover:bg-gray-700 rounded transition-colors"
            title="Add Sample Data"
          >
            <FlaskConical className="w-4 h-4" />
          </button>
          <button
            onClick={handleClearData}
            disabled={actionInProgress}
            className="p-2 text-gray-400 hover:text-red-500 hover:bg-gray-700 rounded transition-colors"
            title="Clear All"
          >
            <Trash2 className="w-4 h-4" />
          </button>
          <button
            onClick={() => {
              refetch();
              refetchStats();
            }}
            disabled={actionInProgress}
            className="p-2 text-gray-400 hover:text-white hover:bg-gray-700 rounded transition-colors"
            title="Refresh"
          >
            <RefreshCw className={`w-4 h-4 ${actionInProgress ? "animate-spin" : ""}`} />
          </button>
        </div>
      </div>

      {/* Compare Mode Banner */}
      {compareMode && (
        <div className="bg-orange-900/30 border-b border-orange-500 px-4 py-2 flex items-center justify-between">
          <div className="flex items-center gap-2 text-sm text-orange-300">
            <GitCompare className="w-4 h-4" />
            <span>
              Compare mode: Select a second checkpoint to compare
              {selectedCheckpointId && compareCheckpointId && " (comparing selected checkpoints)"}
            </span>
          </div>
          <button
            onClick={() => {
              setCompareMode(false);
              setCompareCheckpointId(null);
            }}
            className="p-1 hover:bg-orange-900/50 rounded transition-colors"
          >
            <X className="w-4 h-4 text-orange-300" />
          </button>
        </div>
      )}

      {/* Replay Status Banner */}
      {replayStatus.message && (
        <div
          className={`px-4 py-2 flex items-center justify-between border-b ${
            replayStatus.success
              ? "bg-green-900/30 border-green-500 text-green-300"
              : "bg-red-900/30 border-red-500 text-red-300"
          }`}
        >
          <div className="flex items-center gap-2 text-sm whitespace-pre-line">
            {replayStatus.success ? (
              <Play className="w-4 h-4" />
            ) : (
              <AlertTriangle className="w-4 h-4" />
            )}
            <span>{replayStatus.message}</span>
          </div>
          <button
            onClick={() => setReplayStatus((prev) => ({ ...prev, message: null }))}
            className={`p-1 rounded transition-colors ${
              replayStatus.success ? "hover:bg-green-900/50" : "hover:bg-red-900/50"
            }`}
          >
            <X className="w-4 h-4" />
          </button>
        </div>
      )}

      {/* Lineage Panel (Collapsible) */}
      {showLineage && lineageTaskId && (
        <div className="border-b border-gray-700">
          <div className="h-64 relative">
            <LineageTreeView
              taskId={lineageTaskId}
              onNavigateToTask={handleNavigateToTask}
              expanded={lineageExpanded}
              onClose={() => setLineageExpanded(false)}
            />
            {/* Expand Button */}
            <button
              onClick={() => setLineageExpanded(true)}
              className="absolute bottom-2 right-2 px-2 py-1 bg-gray-700 text-gray-300 rounded text-xs hover:bg-gray-600 transition-colors flex items-center gap-1"
              title="Expand to full screen"
            >
              <svg
                className="w-3 h-3"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
              >
                <path d="M15 3h6v6M9 21H3v-6M21 3l-7 7M3 21l7-7" />
              </svg>
              Expand
            </button>
          </div>
        </div>
      )}

      {/* Lineage Modal (Expanded View) */}
      {lineageExpanded && lineageTaskId && (
        <LineageTreeView
          taskId={lineageTaskId}
          onNavigateToTask={handleNavigateToTask}
          expanded={true}
          onClose={() => setLineageExpanded(false)}
        />
      )}

      {/* Main Content */}
      {isEmpty ? (
        <div className="flex-1 flex items-center justify-center">
          <div className="text-center">
            <Clock className="w-12 h-12 text-gray-600 mx-auto mb-4" />
            <p className="text-gray-400">No checkpoints available</p>
            <p className="text-sm text-gray-500 mt-1">
              Run tasks with checkpointing enabled to see them here
            </p>
            <button
              onClick={handleAddSampleData}
              disabled={actionInProgress}
              className="mt-4 px-4 py-2 bg-cyan-600 text-white rounded hover:bg-cyan-500 transition-colors flex items-center gap-2 mx-auto"
            >
              <FlaskConical className="w-4 h-4" />
              Add Sample Data
            </button>
          </div>
        </div>
      ) : (
        <div className="flex-1 flex overflow-hidden">
          {/* Timeline Panel */}
          <div className="w-96 border-r border-gray-700 overflow-hidden flex flex-col">
            <div className="p-3 border-b border-gray-700 text-sm text-gray-400">
              {checkpoints.length} checkpoints
              {selectedTaskId && ` for ${selectedTaskId}`}
            </div>
            <div className="flex-1 overflow-auto">
              <CheckpointTimeline
                checkpoints={checkpoints}
                selectedId={selectedCheckpointId}
                compareId={compareCheckpointId}
                onSelect={handleSelectCheckpoint}
                onCompare={handleCompareCheckpoint}
                compareMode={compareMode}
                activeId={activeCheckpointId}
              />
            </div>
          </div>

          {/* Detail Panel */}
          <div className="flex-1 overflow-auto p-4">
            {loadingCheckpoint || loadingDiff ? (
              <div className="h-full flex items-center justify-center">
                <RefreshCw className="w-6 h-6 animate-spin text-gray-400" />
              </div>
            ) : compareMode && diff ? (
              <CheckpointDiffView diff={diff} />
            ) : selectedCheckpoint ? (
              <StateSnapshotView checkpoint={selectedCheckpoint} />
            ) : (
              <div className="h-full flex items-center justify-center text-gray-500">
                <div className="text-center">
                  <Clock className="w-12 h-12 mx-auto mb-4 opacity-50" />
                  <p>Select a checkpoint to view details</p>
                </div>
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
