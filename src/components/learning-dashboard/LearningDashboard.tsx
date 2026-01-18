/**
 * LearningDashboard.tsx
 *
 * Main Learning Insights Dashboard page.
 * Shows AI learning patterns, insights, strategy performance, recent tasks, and trends.
 */

import { useState } from "react";
import {
  Brain,
  RefreshCw,
  AlertTriangle,
  Download,
  Upload,
  Trash2,
  FlaskConical,
  ListTodo,
  Loader2,
  Radio,
} from "lucide-react";
import {
  useLearningData,
  learningActions,
  useRecentTasks,
  useLearningStats,
  useCurrentTask,
} from "./useLearningData";
import { LearningSummaryCard } from "./LearningSummaryCard";
import { PatternList } from "./PatternList";
import { InsightList } from "./InsightList";
import { StrategyRankings } from "./StrategyRankings";
import { RecentTasksList } from "./RecentTasksList";

export function LearningDashboard() {
  const { data, isLoading, error, refetch, isRealtimeConnected, isTaskRunning } = useLearningData();
  const { tasks: recentTasks, isLoading: tasksLoading, refetch: refetchTasks } = useRecentTasks(10);
  const { stats, refetch: refetchStats } = useLearningStats();
  const { currentTask, realtimeIteration } = useCurrentTask();
  const [activeTab, setActiveTab] = useState<"patterns" | "insights">("patterns");
  const [actionInProgress, setActionInProgress] = useState(false);

  const handleRefreshAll = async () => {
    await Promise.all([refetch(), refetchTasks(), refetchStats()]);
  };

  const handleAddSampleData = async () => {
    setActionInProgress(true);
    try {
      await learningActions.addSampleData();
      await handleRefreshAll();
    } finally {
      setActionInProgress(false);
    }
  };

  const handleClearData = async () => {
    if (!confirm("Are you sure you want to clear all learning data?")) return;
    setActionInProgress(true);
    try {
      await learningActions.clearData();
      await handleRefreshAll();
    } finally {
      setActionInProgress(false);
    }
  };

  const handleExport = async () => {
    try {
      const json = await learningActions.exportData();
      const blob = new Blob([json], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `learning-data-${new Date().toISOString().slice(0, 10)}.json`;
      a.click();
      URL.revokeObjectURL(url);
    } catch (err) {
      console.error("Export failed:", err);
    }
  };

  const handleImport = async () => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = ".json";
    input.onchange = async (e) => {
      const file = (e.target as HTMLInputElement).files?.[0];
      if (!file) return;
      try {
        const text = await file.text();
        await learningActions.importData(text);
        await handleRefreshAll();
      } catch (err) {
        console.error("Import failed:", err);
        alert("Failed to import learning data. Please check the file format.");
      }
    };
    input.click();
  };

  if (isLoading) {
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
          <p className="text-gray-400">Failed to load learning data</p>
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

  if (!data) {
    return (
      <div className="h-full flex items-center justify-center">
        <div className="text-center">
          <Brain className="w-12 h-12 text-gray-600 mx-auto mb-4" />
          <p className="text-gray-400">No learning data available</p>
          <button
            onClick={handleAddSampleData}
            disabled={actionInProgress}
            className="mt-4 px-4 py-2 bg-purple-600 text-white rounded hover:bg-purple-500 transition-colors flex items-center gap-2 mx-auto"
          >
            <FlaskConical className="w-4 h-4" />
            Add Sample Data
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="h-full overflow-auto p-4 space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Brain className="w-6 h-6 text-purple-500" />
          <h1 className="text-xl font-bold text-white">Learning Insights</h1>
          {/* Live indicator */}
          {isRealtimeConnected && (
            <div className="flex items-center gap-1 ml-2">
              <Radio
                className={`w-3 h-3 ${isTaskRunning ? "text-green-400 animate-pulse" : "text-green-600"}`}
              />
              <span className={`text-xs ${isTaskRunning ? "text-green-400" : "text-green-600"}`}>
                {isTaskRunning ? "LIVE" : "Connected"}
              </span>
            </div>
          )}
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={handleImport}
            disabled={actionInProgress}
            className="p-2 text-gray-400 hover:text-white hover:bg-gray-700 rounded transition-colors"
            title="Import Data"
          >
            <Upload className="w-4 h-4" />
          </button>
          <button
            onClick={handleExport}
            disabled={actionInProgress}
            className="p-2 text-gray-400 hover:text-white hover:bg-gray-700 rounded transition-colors"
            title="Export Data"
          >
            <Download className="w-4 h-4" />
          </button>
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
            title="Clear Data"
          >
            <Trash2 className="w-4 h-4" />
          </button>
          <button
            onClick={handleRefreshAll}
            disabled={actionInProgress}
            className="p-2 text-gray-400 hover:text-white hover:bg-gray-700 rounded transition-colors"
            title="Refresh"
          >
            <RefreshCw className={`w-4 h-4 ${actionInProgress ? "animate-spin" : ""}`} />
          </button>
        </div>
      </div>

      {/* Current Running Task Indicator */}
      {currentTask && (
        <div className="bg-blue-900/30 border border-blue-500/50 rounded-lg p-3">
          <div className="flex items-center gap-2">
            <Loader2 className="w-4 h-4 animate-spin text-blue-400" />
            <span className="text-blue-300 font-medium">Currently Running:</span>
            <span className="text-white">{currentTask.task_name}</span>
            <span className="text-gray-400 text-sm ml-auto">
              {realtimeIteration !== null && (
                <span className="text-blue-300 mr-2">Iteration {realtimeIteration}</span>
              )}
              Session {currentTask.sessions_count}
              {currentTask.max_sessions && ` / ${currentTask.max_sessions}`}
            </span>
          </div>
          {currentTask.prompt && (
            <p className="text-xs text-gray-400 mt-1 ml-6 truncate">
              {currentTask.prompt.slice(0, 100)}
              {currentTask.prompt.length > 100 ? "..." : ""}
            </p>
          )}
        </div>
      )}

      {/* Stats Summary Card */}
      {stats && (
        <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
          <div className="bg-gray-800 rounded-lg p-3">
            <div className="text-2xl font-bold text-white">{stats.total_tasks}</div>
            <div className="text-xs text-gray-400">Total Tasks</div>
          </div>
          <div className="bg-gray-800 rounded-lg p-3">
            <div className="text-2xl font-bold text-green-400">
              {stats.success_rate.toFixed(1)}%
            </div>
            <div className="text-xs text-gray-400">Success Rate</div>
          </div>
          <div className="bg-gray-800 rounded-lg p-3">
            <div className="text-2xl font-bold text-white">
              {stats.avg_duration_secs !== null
                ? `${Math.round(stats.avg_duration_secs / 60)}m`
                : "-"}
            </div>
            <div className="text-xs text-gray-400">Avg Duration</div>
          </div>
          <div className="bg-gray-800 rounded-lg p-3">
            <div className="text-2xl font-bold text-white">
              {stats.avg_iterations !== null ? stats.avg_iterations.toFixed(1) : "-"}
            </div>
            <div className="text-xs text-gray-400">Avg Iterations</div>
          </div>
        </div>
      )}

      {/* Recent Tasks Section */}
      <div className="bg-gray-800 rounded-lg overflow-hidden">
        <div className="flex items-center justify-between px-4 py-3 border-b border-gray-700">
          <div className="flex items-center gap-2">
            <ListTodo className="w-4 h-4 text-cyan-400" />
            <h2 className="text-sm font-medium text-white">Recent Tasks</h2>
            <span className="text-xs text-gray-500">({recentTasks.length})</span>
          </div>
          <button
            onClick={refetchTasks}
            className="text-gray-400 hover:text-white transition-colors"
            title="Refresh tasks"
          >
            <RefreshCw className="w-3 h-3" />
          </button>
        </div>
        <div className="p-3 max-h-80 overflow-y-auto">
          <RecentTasksList tasks={recentTasks} isLoading={tasksLoading} />
        </div>
      </div>

      {/* Summary Card */}
      <LearningSummaryCard summary={data.summary} trend={data.analysis.trend} />

      {/* Strategy Rankings */}
      <StrategyRankings strategies={data.analysis.top_strategies} tools={data.analysis.top_tools} />

      {/* Patterns & Insights Tabs */}
      <div className="bg-gray-800 rounded-lg overflow-hidden">
        <div className="flex border-b border-gray-700">
          <button
            onClick={() => setActiveTab("patterns")}
            className={`flex-1 px-4 py-3 text-sm font-medium transition-colors ${
              activeTab === "patterns"
                ? "text-white bg-gray-700"
                : "text-gray-400 hover:text-white hover:bg-gray-750"
            }`}
          >
            Patterns ({data.patterns.length})
          </button>
          <button
            onClick={() => setActiveTab("insights")}
            className={`flex-1 px-4 py-3 text-sm font-medium transition-colors ${
              activeTab === "insights"
                ? "text-white bg-gray-700"
                : "text-gray-400 hover:text-white hover:bg-gray-750"
            }`}
          >
            Insights ({data.insights.length})
          </button>
        </div>
        <div className="p-4">
          {activeTab === "patterns" ? (
            <PatternList patterns={data.patterns} />
          ) : (
            <InsightList insights={data.insights} />
          )}
        </div>
      </div>
    </div>
  );
}
