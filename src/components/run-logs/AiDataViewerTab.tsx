/**
 * AiDataViewerTab.tsx
 *
 * Displays all data that is accessible to AI via MCP tools.
 * Organized by category: Task Runs | Automation Runs | JSONL Logs
 *
 * This helps users see exactly what data the AI receives.
 */

import { useState } from "react";
import {
  Database,
  Loader2,
  AlertCircle,
  Activity,
  ClipboardList,
  FileJson,
  ChevronDown,
  ChevronRight,
  Clock,
  CheckCircle,
  XCircle,
  Pause,
} from "lucide-react";
import { useExecution } from "../../contexts/ExecutionContext";
import {
  useTaskRuns,
  useAutomationRuns,
  useJsonlLogsSummary,
  useJsonlLogs,
} from "../../hooks/useAiData";
import type { TaskRun, JsonlLogType } from "../../types/aiData";
import type { RunDetails } from "../../types/statistics";

type DataCategory = "task-runs" | "automation-runs" | "jsonl-logs";

interface CategoryTabProps {
  id: DataCategory;
  label: string;
  icon: React.ReactNode;
  active: boolean;
  onClick: () => void;
  count?: number;
}

function CategoryTab({ id: _id, label, icon, active, onClick, count }: CategoryTabProps) {
  return (
    <button
      onClick={onClick}
      className={`flex items-center gap-2 px-4 py-2 text-sm font-medium border-b-2 transition-colors ${
        active
          ? "border-blue-500 text-blue-400"
          : "border-transparent text-muted-foreground hover:text-foreground hover:border-border"
      }`}
    >
      {icon}
      {label}
      {count !== undefined && count > 0 && (
        <span className="px-1.5 py-0.5 text-xs rounded-full bg-muted">{count}</span>
      )}
    </button>
  );
}

function getStatusIcon(status: string) {
  switch (status) {
    case "running":
      return <Loader2 className="w-3 h-3 animate-spin text-blue-400" />;
    case "complete":
    case "completed":
      return <CheckCircle className="w-3 h-3 text-green-400" />;
    case "failed":
      return <XCircle className="w-3 h-3 text-red-400" />;
    case "stopped":
      return <Pause className="w-3 h-3 text-orange-400" />;
    default:
      return <Clock className="w-3 h-3 text-muted-foreground" />;
  }
}

function formatTimestamp(ts: string): string {
  try {
    return new Date(ts).toLocaleString();
  } catch {
    return ts;
  }
}

// Task Runs Section
function TaskRunsSection() {
  const { data: taskRuns, isLoading, error } = useTaskRuns(50);
  const [expandedId, setExpandedId] = useState<string | null>(null);

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin mr-2" />
        Loading task runs...
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex items-center justify-center py-8 text-red-400">
        <AlertCircle className="w-5 h-5 mr-2" />
        Error: {error.message}
      </div>
    );
  }

  if (!taskRuns || taskRuns.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <ClipboardList className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">No task runs found</p>
      </div>
    );
  }

  return (
    <div className="space-y-2">
      {taskRuns.map((run: TaskRun) => (
        <div key={run.id} className="border border-border rounded-lg overflow-hidden">
          <button
            onClick={() => setExpandedId(expandedId === run.id ? null : run.id)}
            className="w-full flex items-center gap-3 px-4 py-3 bg-card hover:bg-muted/50 transition-colors text-left"
          >
            {expandedId === run.id ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground" />
            )}
            {getStatusIcon(run.status)}
            <span className="font-medium truncate flex-1">{run.task_name}</span>
            <span className="text-xs text-muted-foreground">Sessions: {run.sessions_count}</span>
            <span className="text-xs text-muted-foreground">{formatTimestamp(run.created_at)}</span>
          </button>
          {expandedId === run.id && (
            <div className="px-4 py-3 bg-muted/30 border-t border-border space-y-3">
              <div>
                <div className="text-xs font-medium text-muted-foreground mb-1">Prompt</div>
                <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-32 overflow-y-auto">
                  {run.prompt}
                </pre>
              </div>
              {run.output_log && (
                <div>
                  <div className="text-xs font-medium text-muted-foreground mb-1">
                    Output Log ({run.output_log.length} chars)
                  </div>
                  <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-48 overflow-y-auto whitespace-pre-wrap">
                    {run.output_log.slice(-2000)}
                    {run.output_log.length > 2000 && "\n\n... (truncated)"}
                  </pre>
                </div>
              )}
              {run.execution_steps_json && (
                <div>
                  <div className="text-xs font-medium text-muted-foreground mb-1">
                    Execution Steps (JSON)
                  </div>
                  <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-32 overflow-y-auto">
                    {run.execution_steps_json}
                  </pre>
                </div>
              )}
              {run.log_sources_json && (
                <div>
                  <div className="text-xs font-medium text-muted-foreground mb-1">
                    Log Sources (JSON)
                  </div>
                  <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-32 overflow-y-auto">
                    {run.log_sources_json}
                  </pre>
                </div>
              )}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}

// Automation Runs Section
function AutomationRunsSection() {
  const { config } = useExecution();
  const configId = config?.path || "";
  const { data: runs, isLoading, error } = useAutomationRuns(configId, 50);
  const [expandedId, setExpandedId] = useState<string | null>(null);

  if (!configId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Activity className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Load a configuration to view automation runs</p>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin mr-2" />
        Loading automation runs...
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex items-center justify-center py-8 text-red-400">
        <AlertCircle className="w-5 h-5 mr-2" />
        Error: {error.message}
      </div>
    );
  }

  if (!runs || runs.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Activity className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">No automation runs found for this config</p>
      </div>
    );
  }

  return (
    <div className="space-y-2">
      {runs.map((run: RunDetails) => (
        <div key={run.id} className="border border-border rounded-lg overflow-hidden">
          <button
            onClick={() => setExpandedId(expandedId === run.id ? null : run.id)}
            className="w-full flex items-center gap-3 px-4 py-3 bg-card hover:bg-muted/50 transition-colors text-left"
          >
            {expandedId === run.id ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground" />
            )}
            {getStatusIcon(run.status)}
            <span className="font-medium truncate flex-1">
              {run.workflow_name || "Unknown Workflow"}
            </span>
            {run.duration_ms !== undefined && (
              <span className="text-xs text-muted-foreground">
                {(run.duration_ms / 1000).toFixed(1)}s
              </span>
            )}
            <span className="text-xs text-muted-foreground">{formatTimestamp(run.started_at)}</span>
          </button>
          {expandedId === run.id && (
            <div className="px-4 py-3 bg-muted/30 border-t border-border space-y-3">
              {run.actions_summary && (
                <div>
                  <div className="text-xs font-medium text-muted-foreground mb-1">
                    Actions Summary
                  </div>
                  <pre className="text-xs bg-background p-2 rounded overflow-x-auto">
                    {JSON.stringify(run.actions_summary, null, 2)}
                  </pre>
                </div>
              )}
              {run.states_visited && run.states_visited.length > 0 && (
                <div>
                  <div className="text-xs font-medium text-muted-foreground mb-1">
                    States Visited ({run.states_visited.length})
                  </div>
                  <pre className="text-xs bg-background p-2 rounded overflow-x-auto">
                    {JSON.stringify(run.states_visited, null, 2)}
                  </pre>
                </div>
              )}
              {run.transitions_executed && run.transitions_executed.length > 0 && (
                <div>
                  <div className="text-xs font-medium text-muted-foreground mb-1">
                    Transitions Executed ({run.transitions_executed.length})
                  </div>
                  <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-48 overflow-y-auto">
                    {JSON.stringify(run.transitions_executed, null, 2)}
                  </pre>
                </div>
              )}
              {run.template_matches && run.template_matches.length > 0 && (
                <div>
                  <div className="text-xs font-medium text-muted-foreground mb-1">
                    Template Matches ({run.template_matches.length})
                  </div>
                  <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-48 overflow-y-auto">
                    {JSON.stringify(run.template_matches, null, 2)}
                  </pre>
                </div>
              )}
              {run.anomalies && run.anomalies.length > 0 && (
                <div>
                  <div className="text-xs font-medium text-muted-foreground mb-1">
                    Anomalies ({run.anomalies.length})
                  </div>
                  <pre className="text-xs bg-background p-2 rounded overflow-x-auto">
                    {JSON.stringify(run.anomalies, null, 2)}
                  </pre>
                </div>
              )}
              {run.error_message && (
                <div>
                  <div className="text-xs font-medium text-red-400 mb-1">Error</div>
                  <pre className="text-xs bg-red-500/10 text-red-400 p-2 rounded overflow-x-auto">
                    {run.error_message}
                  </pre>
                </div>
              )}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}

// JSONL Logs Section
function JsonlLogsSection() {
  const { data: summary, isLoading: summaryLoading } = useJsonlLogsSummary();
  const [activeLogType, setActiveLogType] = useState<JsonlLogType>("general");
  const { data: logs, isLoading: logsLoading, error } = useJsonlLogs(activeLogType, 100);

  const logTypes: { type: JsonlLogType; label: string }[] = [
    { type: "general", label: "General" },
    { type: "actions", label: "Actions" },
    { type: "image-recognition", label: "Image Recognition" },
    { type: "playwright", label: "Playwright" },
    { type: "ai-output", label: "AI Output" },
  ];

  const getLogCount = (type: JsonlLogType): number => {
    if (!summary) return 0;
    switch (type) {
      case "general":
        return summary.general.entry_count;
      case "actions":
        return summary.actions.entry_count;
      case "image-recognition":
        return summary.image_recognition.entry_count;
      case "playwright":
        return summary.playwright.entry_count;
      case "ai-output":
        return summary.ai_output.entry_count;
      default:
        return 0;
    }
  };

  return (
    <div className="space-y-4">
      {/* Log type tabs */}
      <div className="flex gap-2 flex-wrap">
        {logTypes.map(({ type, label }) => (
          <button
            key={type}
            onClick={() => setActiveLogType(type)}
            className={`px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
              activeLogType === type
                ? "bg-blue-500/20 text-blue-400 border border-blue-500/30"
                : "bg-muted text-muted-foreground hover:text-foreground"
            }`}
          >
            {label}
            {!summaryLoading && <span className="ml-1.5 opacity-60">({getLogCount(type)})</span>}
          </button>
        ))}
      </div>

      {/* Log content */}
      {logsLoading ? (
        <div className="flex items-center justify-center py-8 text-muted-foreground">
          <Loader2 className="w-5 h-5 animate-spin mr-2" />
          Loading logs...
        </div>
      ) : error ? (
        <div className="flex items-center justify-center py-8 text-red-400">
          <AlertCircle className="w-5 h-5 mr-2" />
          Error: {error.message}
        </div>
      ) : logs && logs.entries.length > 0 ? (
        <div className="space-y-2">
          <div className="text-xs text-muted-foreground mb-2">
            Showing {logs.count} entries from {logs.file_path}
          </div>
          <div className="border border-border rounded-lg overflow-hidden">
            <pre className="text-xs bg-background p-3 overflow-x-auto max-h-[500px] overflow-y-auto">
              {logs.entries.map((entry, i) => (
                <div key={i} className="py-1 border-b border-border/50 last:border-0">
                  {JSON.stringify(entry, null, 2)}
                </div>
              ))}
            </pre>
          </div>
        </div>
      ) : (
        <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
          <FileJson className="w-8 h-8 mb-3 opacity-50" />
          <p className="text-sm">No {activeLogType} log entries found</p>
        </div>
      )}
    </div>
  );
}

export function AiDataViewerTab() {
  const [activeCategory, setActiveCategory] = useState<DataCategory>("task-runs");
  const { data: taskRuns } = useTaskRuns(50);
  const { config } = useExecution();
  const configId = config?.path || "";
  const { data: automationRuns } = useAutomationRuns(configId, 50);
  const { data: logsSummary } = useJsonlLogsSummary();

  const getTotalLogCount = (): number => {
    if (!logsSummary) return 0;
    return (
      logsSummary.general.entry_count +
      logsSummary.actions.entry_count +
      logsSummary.image_recognition.entry_count +
      logsSummary.playwright.entry_count +
      logsSummary.ai_output.entry_count
    );
  };

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Header */}
      <div className="flex-shrink-0 bg-background border-b border-border">
        <div className="flex items-center justify-between px-4 py-3">
          <div className="flex items-center gap-2">
            <Database className="w-4 h-4 text-muted-foreground" />
            <span className="font-medium">AI Data Viewer</span>
          </div>
          <span className="text-xs text-muted-foreground">Data accessible to AI via MCP</span>
        </div>

        {/* Category tabs */}
        <div className="flex px-4 gap-4">
          <CategoryTab
            id="task-runs"
            label="Task Runs"
            icon={<ClipboardList className="w-4 h-4" />}
            active={activeCategory === "task-runs"}
            onClick={() => setActiveCategory("task-runs")}
            count={taskRuns?.length}
          />
          <CategoryTab
            id="automation-runs"
            label="Automation Runs"
            icon={<Activity className="w-4 h-4" />}
            active={activeCategory === "automation-runs"}
            onClick={() => setActiveCategory("automation-runs")}
            count={automationRuns?.length}
          />
          <CategoryTab
            id="jsonl-logs"
            label="JSONL Logs"
            icon={<FileJson className="w-4 h-4" />}
            active={activeCategory === "jsonl-logs"}
            onClick={() => setActiveCategory("jsonl-logs")}
            count={getTotalLogCount()}
          />
        </div>
      </div>

      {/* Content */}
      <div className="flex-1 min-h-0 overflow-auto p-4">
        {activeCategory === "task-runs" && <TaskRunsSection />}
        {activeCategory === "automation-runs" && <AutomationRunsSection />}
        {activeCategory === "jsonl-logs" && <JsonlLogsSection />}
      </div>
    </div>
  );
}

export default AiDataViewerTab;
