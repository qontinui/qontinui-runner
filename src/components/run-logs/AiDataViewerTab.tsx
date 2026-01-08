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
  Loader2,
  AlertCircle,
  Activity,
  FileJson,
  FileText,
  ChevronDown,
  ChevronRight,
  Clock,
  CheckCircle,
  XCircle,
  Pause,
  Image,
  Settings,
  MessageSquare,
  BookOpen,
} from "lucide-react";
import { useExecution } from "../../contexts/ExecutionContext";
import { useRunSelection } from "../../contexts/RunSelectionContext";
import {
  useAutomationRuns,
  useJsonlLogsForTaskRun,
  useConsolidatedAiOutput,
  useTextLogsSummary,
  useTextLogs,
  useScreenshots,
  useLoadedConfig,
  useAiPrompts,
  useContexts,
} from "../../hooks/useAiData";
import type {
  JsonlLogType,
  TextLogType,
  ScreenshotInfo,
  ContextInfo,
  AiOutputChunk,
} from "../../types/aiData";
import type { RunDetails } from "../../types/statistics";
import { MarkdownViewer } from "../MarkdownViewer";

type DataCategory =
  | "ai-prompt"
  | "jsonl-logs"
  | "dev-logs"
  | "automation-runs"
  | "screenshots"
  | "loaded-config"
  | "contexts";

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

// Consolidated AI Output display component
function AiOutputChunkItem({
  chunk,
  defaultExpanded,
}: {
  chunk: AiOutputChunk;
  defaultExpanded: boolean;
}) {
  const [isExpanded, setIsExpanded] = useState(defaultExpanded);

  return (
    <div className="border border-border rounded-lg overflow-hidden transition-all duration-200">
      <button
        onClick={() => setIsExpanded(!isExpanded)}
        className="w-full flex items-center gap-2 px-3 py-2 bg-muted/50 border-b border-border hover:bg-muted/80 transition-colors text-left"
      >
        {isExpanded ? (
          <ChevronDown className="w-4 h-4 text-muted-foreground flex-shrink-0" />
        ) : (
          <ChevronRight className="w-4 h-4 text-muted-foreground flex-shrink-0" />
        )}
        <span
          className={`px-2 py-0.5 text-xs font-medium rounded ${
            chunk.source === "claude"
              ? "bg-blue-500/20 text-blue-400"
              : chunk.source === "prompt"
                ? "bg-green-500/20 text-green-400"
                : "bg-muted text-muted-foreground"
          }`}
        >
          {chunk.source}
        </span>
        <span className="text-xs text-muted-foreground">
          [{chunk.start_time}
          {chunk.end_time ? ` - ${chunk.end_time}` : ""}]
        </span>
        <span className="text-xs text-muted-foreground flex-1">
          ({chunk.entry_count} {chunk.entry_count === 1 ? "line" : "lines"})
        </span>
        {!isExpanded && (
          <span className="text-xs text-muted-foreground italic opacity-70">Click to expand</span>
        )}
      </button>
      {isExpanded && <MarkdownViewer content={chunk.content} isAnimated />}
    </div>
  );
}

// Consolidated AI Output display component
function ConsolidatedAiOutputDisplay({
  chunks,
  totalEntries,
}: {
  chunks: AiOutputChunk[];
  totalEntries: number;
}) {
  // Heuristic: If there's only one chunk, show it expanded.
  // If there are multiple, check if they will all fit on the screen.
  // Calculation:
  // - Line height ~ 18px (text-xs + line-height)
  // - Header per chunk ~ 45px
  // - Padding/Other UI ~ 300px
  const LINE_HEIGHT_PX = 18;
  const HEADER_HEIGHT_PX = 45;
  const BASE_UI_HEIGHT_PX = 300;

  const totalLines = chunks.reduce((acc, chunk) => acc + chunk.entry_count, 0);
  const estimatedTotalHeight =
    totalLines * LINE_HEIGHT_PX + chunks.length * HEADER_HEIGHT_PX + BASE_UI_HEIGHT_PX;

  const shouldDefaultExpand = chunks.length <= 1 || estimatedTotalHeight < window.innerHeight;

  return (
    <div className="space-y-4">
      <div className="text-xs text-muted-foreground mb-2">
        {chunks.length} chunks from {totalEntries} raw entries
      </div>
      {chunks.map((chunk, i) => (
        <AiOutputChunkItem key={i} chunk={chunk} defaultExpanded={shouldDefaultExpand} />
      ))}
    </div>
  );
}

// JSONL Logs Section (filtered by task run time range)
function JsonlLogsSection() {
  const { selectedRunId, selectedRun } = useRunSelection();
  const [activeLogType, setActiveLogType] = useState<JsonlLogType>("ai-output");

  // Use consolidated format for AI output, raw JSONL for others
  const {
    data: consolidatedOutput,
    isLoading: consolidatedLoading,
    error: consolidatedError,
  } = useConsolidatedAiOutput(activeLogType === "ai-output" ? selectedRunId : null);
  const {
    data: logs,
    isLoading: logsLoading,
    error: logsError,
  } = useJsonlLogsForTaskRun(
    activeLogType !== "ai-output" ? activeLogType : "general",
    activeLogType !== "ai-output" ? selectedRunId : null,
  );

  const isLoading = activeLogType === "ai-output" ? consolidatedLoading : logsLoading;
  const error = activeLogType === "ai-output" ? consolidatedError : logsError;

  const logTypes: { type: JsonlLogType; label: string }[] = [
    { type: "ai-output", label: "AI Output" },
    { type: "general", label: "General" },
    { type: "actions", label: "Actions" },
    { type: "image-recognition", label: "Image Recognition" },
    { type: "playwright", label: "Playwright" },
  ];

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <FileJson className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view JSONL logs</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {/* Show time range info */}
      {selectedRun && (
        <div className="text-xs text-muted-foreground bg-muted/30 px-3 py-2 rounded-md">
          <span className="font-medium">Time range:</span> {formatTimestamp(selectedRun.created_at)}
          {selectedRun.completed_at
            ? ` → ${formatTimestamp(selectedRun.completed_at)}`
            : " → (still running)"}
        </div>
      )}

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
          </button>
        ))}
      </div>

      {/* Log content */}
      {isLoading ? (
        <div className="flex items-center justify-center py-8 text-muted-foreground">
          <Loader2 className="w-5 h-5 animate-spin mr-2" />
          Loading logs...
        </div>
      ) : error ? (
        <div className="flex items-center justify-center py-8 text-red-400">
          <AlertCircle className="w-5 h-5 mr-2" />
          Error: {error.message}
        </div>
      ) : activeLogType === "ai-output" ? (
        // Consolidated AI Output display
        consolidatedOutput && consolidatedOutput.chunks.length > 0 ? (
          <ConsolidatedAiOutputDisplay
            chunks={consolidatedOutput.chunks}
            totalEntries={consolidatedOutput.total_entries}
          />
        ) : (
          <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
            <FileJson className="w-8 h-8 mb-3 opacity-50" />
            <p className="text-sm">No AI output during this task run</p>
          </div>
        )
      ) : logs && logs.entries.length > 0 ? (
        // Other log types - show raw entries in more readable format
        <div className="space-y-2">
          <div className="text-xs text-muted-foreground mb-2">
            Showing {logs.count} entries during this task run
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
          <p className="text-sm">No {activeLogType} log entries during this task run</p>
        </div>
      )}
    </div>
  );
}

// Dev Logs Section (text logs filtered by task run time range)
function DevLogsSection() {
  const { selectedRunId, selectedRun } = useRunSelection();
  const [activeLogType, setActiveLogType] = useState<TextLogType>("backend");

  const { data: summary, isLoading: summaryLoading } = useTextLogsSummary(selectedRunId);
  const { data: logs, isLoading: logsLoading, error } = useTextLogs(activeLogType, selectedRunId);

  const logTypes: { type: TextLogType; label: string }[] = [
    { type: "backend", label: "Backend" },
    { type: "backend-err", label: "Backend Errors" },
    { type: "qontinui-api", label: "Qontinui API" },
    { type: "qontinui-api-err", label: "API Errors" },
  ];

  const getLogCount = (type: TextLogType): number => {
    if (!summary) return 0;
    const logInfo = summary.logs.find((l) => l.log_type === type);
    return logInfo?.line_count ?? 0;
  };

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <FileText className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view logs</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {/* Show time range info */}
      {selectedRun && (
        <div className="text-xs text-muted-foreground bg-muted/30 px-3 py-2 rounded-md">
          <span className="font-medium">Time range:</span> {formatTimestamp(selectedRun.created_at)}
          {selectedRun.completed_at
            ? ` → ${formatTimestamp(selectedRun.completed_at)}`
            : " → (still running)"}
        </div>
      )}

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
      ) : logs && logs.content ? (
        <div className="space-y-2">
          <div className="text-xs text-muted-foreground mb-2">
            Showing {logs.line_count} log lines from {logs.file_path}
          </div>
          <div className="border border-border rounded-lg overflow-hidden">
            <pre className="text-xs bg-background p-3 overflow-x-auto max-h-[500px] overflow-y-auto font-mono whitespace-pre-wrap">
              {logs.content}
            </pre>
          </div>
        </div>
      ) : (
        <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
          <FileText className="w-8 h-8 mb-3 opacity-50" />
          <p className="text-sm">No {activeLogType} log entries during this task run</p>
        </div>
      )}
    </div>
  );
}

// Screenshots Section
function ScreenshotsSection() {
  const { data: screenshots, isLoading, error } = useScreenshots();
  const [selectedImage, setSelectedImage] = useState<string | null>(null);

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin mr-2" />
        Loading screenshots...
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

  const hasScreenshots =
    screenshots && (screenshots.annotated.length > 0 || screenshots.playwright.length > 0);

  if (!hasScreenshots) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Image className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">No screenshots found</p>
      </div>
    );
  }

  const renderScreenshotList = (items: ScreenshotInfo[], title: string) => {
    if (items.length === 0) return null;
    return (
      <div className="space-y-2">
        <h3 className="text-sm font-medium text-muted-foreground">{title}</h3>
        <div className="grid grid-cols-2 md:grid-cols-3 lg:grid-cols-4 gap-3">
          {items.map((img) => (
            <button
              key={img.path}
              onClick={() => setSelectedImage(img.path)}
              className="border border-border rounded-lg overflow-hidden hover:border-blue-500/50 transition-colors"
            >
              <div className="aspect-video bg-muted flex items-center justify-center">
                <Image className="w-8 h-8 text-muted-foreground opacity-50" />
              </div>
              <div className="p-2">
                <div className="text-xs font-medium truncate">{img.filename}</div>
                <div className="text-xs text-muted-foreground">
                  {(img.size_bytes / 1024).toFixed(1)} KB
                </div>
              </div>
            </button>
          ))}
        </div>
      </div>
    );
  };

  return (
    <div className="space-y-6">
      {renderScreenshotList(screenshots.annotated, "Annotated Screenshots")}
      {renderScreenshotList(screenshots.playwright, "Playwright Screenshots")}

      {/* Image preview modal */}
      {selectedImage && (
        <div
          className="fixed inset-0 bg-black/80 flex items-center justify-center z-50"
          onClick={() => setSelectedImage(null)}
        >
          <div className="max-w-[90vw] max-h-[90vh] overflow-auto">
            <div className="text-xs text-muted-foreground mb-2">{selectedImage}</div>
            <div className="bg-muted p-4 rounded-lg text-center text-muted-foreground">
              Image preview not available in this view.
              <br />
              Use the Read tool to view: {selectedImage}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

// Loaded Config Section
function LoadedConfigSection() {
  const { data: config, isLoading, error } = useLoadedConfig();

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin mr-2" />
        Loading config...
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

  if (!config || !config.config_content) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Settings className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">No config loaded</p>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full space-y-4">
      {config.meta && (
        <div className="text-xs text-muted-foreground bg-muted/30 px-3 py-2 rounded-md space-y-1 flex-shrink-0">
          {config.meta.source_path && (
            <div>
              <span className="font-medium">Source:</span> {config.meta.source_path}
            </div>
          )}
          {config.meta.loaded_at && (
            <div>
              <span className="font-medium">Loaded at:</span>{" "}
              {formatTimestamp(config.meta.loaded_at)}
            </div>
          )}
          {config.config_format && (
            <div>
              <span className="font-medium">Format:</span> {config.config_format.toUpperCase()}
            </div>
          )}
        </div>
      )}

      <div className="flex-1 min-h-0 border border-border rounded-lg overflow-hidden">
        <pre className="text-xs bg-background p-3 h-full overflow-y-auto font-mono whitespace-pre">
          {config.config_content}
        </pre>
      </div>
    </div>
  );
}

// AI Prompt Section (shows the prompt directly for the selected task run)
function AiPromptSection() {
  const { selectedRunId, selectedRun } = useRunSelection();
  const { data: prompts, isLoading, error } = useAiPrompts(selectedRunId);

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <MessageSquare className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view the AI prompt</p>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin mr-2" />
        Loading prompt...
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

  // Get the first (main) prompt - usually there's one prompt per task run
  const mainPrompt = prompts?.prompts?.[0];

  if (!mainPrompt) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <MessageSquare className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">No AI prompt found for this task run</p>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full space-y-4">
      {/* Show task run info */}
      {selectedRun && (
        <div className="text-xs text-muted-foreground bg-muted/30 px-3 py-2 rounded-md space-y-1 flex-shrink-0">
          <div>
            <span className="font-medium">Task:</span> {selectedRun.task_name}
          </div>
          <div>
            <span className="font-medium">Created:</span> {formatTimestamp(selectedRun.created_at)}
          </div>
          <div>
            <span className="font-medium">Prompt size:</span>{" "}
            {(mainPrompt.content.length / 1024).toFixed(1)} KB
          </div>
        </div>
      )}

      {/* Show the prompt content directly */}
      <div className="flex-1 min-h-0 border border-border rounded-lg overflow-hidden flex flex-col">
        <div className="flex-1 min-h-0 overflow-y-auto">
          <MarkdownViewer content={mainPrompt.content} className="min-h-full" />
        </div>
      </div>
    </div>
  );
}

// Contexts Section
function ContextsSection() {
  const { data: contextsData, isLoading, error } = useContexts();
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [filter, setFilter] = useState<"all" | "user" | "builtin">("all");

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin mr-2" />
        Loading contexts...
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

  if (!contextsData || contextsData.contexts.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <BookOpen className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">No contexts available</p>
      </div>
    );
  }

  const filteredContexts = contextsData.contexts.filter((ctx: ContextInfo) => {
    if (filter === "all") return true;
    return ctx.context_type === filter;
  });

  const userCount = contextsData.contexts.filter(
    (c: ContextInfo) => c.context_type === "user",
  ).length;
  const builtinCount = contextsData.contexts.filter(
    (c: ContextInfo) => c.context_type === "builtin",
  ).length;

  return (
    <div className="space-y-4">
      {/* Filter tabs */}
      <div className="flex gap-2">
        <button
          onClick={() => setFilter("all")}
          className={`px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
            filter === "all"
              ? "bg-blue-500/20 text-blue-400 border border-blue-500/30"
              : "bg-muted text-muted-foreground hover:text-foreground"
          }`}
        >
          All ({contextsData.contexts.length})
        </button>
        <button
          onClick={() => setFilter("user")}
          className={`px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
            filter === "user"
              ? "bg-blue-500/20 text-blue-400 border border-blue-500/30"
              : "bg-muted text-muted-foreground hover:text-foreground"
          }`}
        >
          User ({userCount})
        </button>
        <button
          onClick={() => setFilter("builtin")}
          className={`px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
            filter === "builtin"
              ? "bg-blue-500/20 text-blue-400 border border-blue-500/30"
              : "bg-muted text-muted-foreground hover:text-foreground"
          }`}
        >
          Built-in ({builtinCount})
        </button>
      </div>

      {/* Context list */}
      <div className="space-y-2">
        {filteredContexts.map((ctx: ContextInfo) => (
          <div key={ctx.id} className="border border-border rounded-lg overflow-hidden">
            <button
              onClick={() => setExpandedId(expandedId === ctx.id ? null : ctx.id)}
              className="w-full flex items-center gap-3 px-4 py-3 bg-card hover:bg-muted/50 transition-colors text-left"
            >
              {expandedId === ctx.id ? (
                <ChevronDown className="w-4 h-4 text-muted-foreground" />
              ) : (
                <ChevronRight className="w-4 h-4 text-muted-foreground" />
              )}
              <BookOpen
                className={`w-4 h-4 ${
                  ctx.context_type === "builtin" ? "text-purple-400" : "text-green-400"
                }`}
              />
              <span className="font-medium flex-1 truncate">{ctx.name}</span>
              {ctx.category && (
                <span className="px-2 py-0.5 text-xs rounded-full bg-muted">{ctx.category}</span>
              )}
              <span
                className={`px-2 py-0.5 text-xs rounded-full ${
                  ctx.context_type === "builtin"
                    ? "bg-purple-500/20 text-purple-400"
                    : "bg-green-500/20 text-green-400"
                }`}
              >
                {ctx.context_type}
              </span>
              <span
                className={`px-2 py-0.5 text-xs rounded-full ${
                  ctx.enabled ? "bg-green-500/20 text-green-400" : "bg-muted text-muted-foreground"
                }`}
              >
                {ctx.enabled ? "enabled" : "disabled"}
              </span>
            </button>
            {expandedId === ctx.id && (
              <div className="px-4 py-3 bg-muted/30 border-t border-border space-y-3">
                {ctx.tags.length > 0 && (
                  <div className="flex gap-1 flex-wrap">
                    {ctx.tags.map((tag) => (
                      <span
                        key={tag}
                        className="px-2 py-0.5 text-xs rounded-full bg-muted text-muted-foreground"
                      >
                        {tag}
                      </span>
                    ))}
                  </div>
                )}
                {ctx.auto_include && (
                  <div>
                    <div className="text-xs font-medium text-muted-foreground mb-1">
                      Auto-include Rules
                    </div>
                    <pre className="text-xs bg-background p-2 rounded overflow-x-auto">
                      {JSON.stringify(ctx.auto_include, null, 2)}
                    </pre>
                  </div>
                )}
                <div>
                  <div className="text-xs font-medium text-muted-foreground mb-1">
                    Content ({(ctx.content.length / 1024).toFixed(1)} KB)
                  </div>
                  <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-64 overflow-y-auto whitespace-pre-wrap">
                    {ctx.content}
                  </pre>
                </div>
              </div>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

export function AiDataViewerTab() {
  const [activeCategory, setActiveCategory] = useState<DataCategory>("ai-prompt");
  const { config } = useExecution();
  const configId = config?.path || "";
  const { data: automationRuns } = useAutomationRuns(configId, 50);

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Category tabs - header is provided by RunPageLayout */}
      <div className="flex-shrink-0 bg-background border-b border-border">
        <div className="flex px-4 gap-4 overflow-x-auto">
          <CategoryTab
            id="ai-prompt"
            label="AI Prompt"
            icon={<MessageSquare className="w-4 h-4" />}
            active={activeCategory === "ai-prompt"}
            onClick={() => setActiveCategory("ai-prompt")}
          />
          <CategoryTab
            id="jsonl-logs"
            label="JSONL Logs"
            icon={<FileJson className="w-4 h-4" />}
            active={activeCategory === "jsonl-logs"}
            onClick={() => setActiveCategory("jsonl-logs")}
          />
          <CategoryTab
            id="dev-logs"
            label="Dev Logs"
            icon={<FileText className="w-4 h-4" />}
            active={activeCategory === "dev-logs"}
            onClick={() => setActiveCategory("dev-logs")}
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
            id="screenshots"
            label="Screenshots"
            icon={<Image className="w-4 h-4" />}
            active={activeCategory === "screenshots"}
            onClick={() => setActiveCategory("screenshots")}
          />
          <CategoryTab
            id="loaded-config"
            label="Loaded Config"
            icon={<Settings className="w-4 h-4" />}
            active={activeCategory === "loaded-config"}
            onClick={() => setActiveCategory("loaded-config")}
          />
          <CategoryTab
            id="contexts"
            label="Contexts"
            icon={<BookOpen className="w-4 h-4" />}
            active={activeCategory === "contexts"}
            onClick={() => setActiveCategory("contexts")}
          />
        </div>
      </div>

      {/* Content */}
      <div
        className={`flex-1 min-h-0 p-4 ${
          ["ai-prompt", "loaded-config"].includes(activeCategory)
            ? "overflow-hidden flex flex-col"
            : "overflow-auto"
        }`}
      >
        {activeCategory === "ai-prompt" && <AiPromptSection />}
        {activeCategory === "jsonl-logs" && <JsonlLogsSection />}
        {activeCategory === "dev-logs" && <DevLogsSection />}
        {activeCategory === "automation-runs" && <AutomationRunsSection />}
        {activeCategory === "screenshots" && <ScreenshotsSection />}
        {activeCategory === "loaded-config" && <LoadedConfigSection />}
        {activeCategory === "contexts" && <ContextsSection />}
      </div>
    </div>
  );
}

export default AiDataViewerTab;
