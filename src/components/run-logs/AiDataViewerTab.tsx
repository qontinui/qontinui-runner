/**
 * AiDataViewerTab.tsx
 *
 * Displays all data that is accessible to AI via MCP tools.
 * Organized into grouped sidebar navigation:
 * - AI Session: Prompt, AI Output, Contexts
 * - Execution Logs: Events, API Requests, Image Recognition, Playwright
 * - Captures: DOM Snapshots
 * - Configuration: Loaded Config, Dev Logs
 *
 * This helps users see exactly what data the AI receives.
 */

import { useState, useCallback } from "react";
import {
  Loader2,
  AlertCircle,
  Activity,
  FileJson,
  FileText,
  ChevronDown,
  ChevronRight,
  CheckCircle,
  XCircle,
  Settings,
  MessageSquare,
  BookOpen,
  Code,
  Globe,
  Eye,
  Cpu,
  Camera,
  Cog,
  Sparkles,
  List,
  TestTube,
  Bot,
  Smartphone,
  Terminal,
  Wifi,
  Timer,
  Clock,
} from "lucide-react";

const EVENTS_PAGE_SIZE = 50;
import { isDevelopmentMode } from "qontinui-navigation";
import { useRunSelection } from "../../contexts/RunSelectionContext";
import {
  useJsonlLogsForTaskRun,
  useConsolidatedAiOutput,
  useTextLogsSummary,
  useTextLogs,
  useLoadedConfig,
  useAiPrompts,
  useContexts,
  useTaskRunEvents,
  useTaskRunMigratedLogsSummary,
} from "../../hooks/useAiData";
import type {
  JsonlLogType,
  TextLogType,
  ContextInfo,
  AiOutputChunk,
  TaskRunEvent,
} from "../../types/aiData";
import { MarkdownViewer } from "../MarkdownViewer";
import { DomSnapshotsPanel } from "../dom-captures";
import { getStatusColors, getAccentColors } from "@/design-system";
import { formatTimestamp } from "./ai-data-viewer-utils";
import { PlaywrightResultsDisplay } from "./PlaywrightResultsDisplay";
import { ApiRequestsDisplay } from "./ApiRequestsDisplay";
import { ExecutionSpansSection } from "./ExecutionSpansSection";
import {
  MobileStateSection,
  MobileScreenshotsSection,
  MobileLogsSection,
  MobileErrorsSection,
} from "./MobileSections";
import { ProcessSessionsSection, ProcessSessionOutputSection } from "./ProcessSections";
import { McpCallsDisplay } from "./McpCallsDisplay";
import { AwasStepsSection } from "./AwasStepsSection";

type DataCategory =
  | "ai-prompt"
  | "ai-output"
  | "contexts"
  | "events"
  | "api-requests"
  | "mcp-calls"
  | "image-recognition"
  | "playwright-tests"
  | "awas-steps"
  | "execution-spans"
  | "dom-snapshots"
  | "mobile-state"
  | "mobile-screenshots"
  | "mobile-logs"
  | "mobile-errors"
  | "process-sessions"
  | "process-output"
  | "loaded-config"
  | "dev-logs";

interface NavGroup {
  id: string;
  label: string;
  icon: React.ReactNode;
  items: NavItem[];
}

interface NavItem {
  id: DataCategory;
  label: string;
  icon: React.ReactNode;
}

function SidebarNav({
  activeCategory,
  onCategoryChange,
}: {
  activeCategory: DataCategory;
  onCategoryChange: (category: DataCategory) => void;
}) {
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(
    new Set(["ai-session", "execution-logs", "captures", "mobile", "processes", "configuration"]),
  );

  const toggleGroup = (groupId: string) => {
    setExpandedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(groupId)) {
        next.delete(groupId);
      } else {
        next.add(groupId);
      }
      return next;
    });
  };

  const isDevMode = isDevelopmentMode();

  const navGroups: NavGroup[] = [
    {
      id: "ai-session",
      label: "AI Session",
      icon: <Sparkles className="w-4 h-4" />,
      items: [
        {
          id: "ai-prompt",
          label: "Prompt",
          icon: <MessageSquare className="w-4 h-4" />,
        },
        {
          id: "ai-output",
          label: "AI Output",
          icon: <FileText className="w-4 h-4" />,
        },
        {
          id: "contexts",
          label: "Contexts",
          icon: <BookOpen className="w-4 h-4" />,
        },
      ],
    },
    {
      id: "execution-logs",
      label: "Execution Logs",
      icon: <Cpu className="w-4 h-4" />,
      items: [
        {
          id: "events",
          label: "Events",
          icon: <List className="w-4 h-4" />,
        },
        {
          id: "api-requests",
          label: "API Requests",
          icon: <Globe className="w-4 h-4" />,
        },
        {
          id: "mcp-calls",
          label: "MCP Calls",
          icon: <Wifi className="w-4 h-4" />,
        },
        ...(isDevMode
          ? [
              {
                id: "image-recognition" as DataCategory,
                label: "Image Recognition",
                icon: <Eye className="w-4 h-4" />,
              },
            ]
          : []),
        {
          id: "playwright-tests",
          label: "Playwright Tests",
          icon: <TestTube className="w-4 h-4" />,
        },
        ...(isDevMode
          ? [
              {
                id: "awas-steps" as DataCategory,
                label: "AWAS Steps",
                icon: <Bot className="w-4 h-4" />,
              },
            ]
          : []),
        {
          id: "execution-spans",
          label: "Execution Spans",
          icon: <Timer className="w-4 h-4" />,
        },
      ],
    },
    {
      id: "captures",
      label: "Captures",
      icon: <Camera className="w-4 h-4" />,
      items: [
        {
          id: "dom-snapshots",
          label: "DOM Snapshots",
          icon: <Code className="w-4 h-4" />,
        },
      ],
    },
    ...(isDevMode
      ? [
          {
            id: "mobile",
            label: "Mobile",
            icon: <Smartphone className="w-4 h-4" />,
            items: [
              {
                id: "mobile-state" as DataCategory,
                label: "App State",
                icon: <Activity className="w-4 h-4" />,
              },
              {
                id: "mobile-screenshots" as DataCategory,
                label: "Screenshots",
                icon: <Camera className="w-4 h-4" />,
              },
              {
                id: "mobile-logs" as DataCategory,
                label: "Logs",
                icon: <Terminal className="w-4 h-4" />,
              },
              {
                id: "mobile-errors" as DataCategory,
                label: "Errors",
                icon: <AlertCircle className="w-4 h-4" />,
              },
            ],
          },
        ]
      : []),
    {
      id: "processes",
      label: "Processes",
      icon: <Terminal className="w-4 h-4" />,
      items: [
        {
          id: "process-sessions" as DataCategory,
          label: "Sessions",
          icon: <Clock className="w-4 h-4" />,
        },
        {
          id: "process-output" as DataCategory,
          label: "Output",
          icon: <FileText className="w-4 h-4" />,
        },
      ],
    },
    {
      id: "configuration",
      label: "Configuration",
      icon: <Cog className="w-4 h-4" />,
      items: [
        {
          id: "loaded-config",
          label: "Loaded Config",
          icon: <Settings className="w-4 h-4" />,
        },
        {
          id: "dev-logs",
          label: "Dev Logs",
          icon: <FileJson className="w-4 h-4" />,
        },
      ],
    },
  ];

  return (
    <div className="w-56 shrink-0 border-r border-border bg-muted/30 overflow-y-auto">
      <div className="p-2 space-y-1">
        {navGroups.map((group) => {
          const isExpanded = expandedGroups.has(group.id);
          const hasActiveItem = group.items.some((item) => item.id === activeCategory);

          return (
            <div key={group.id}>
              <button
                onClick={() => toggleGroup(group.id)}
                className={`w-full flex items-center gap-2 px-3 py-2 text-sm font-medium rounded-md transition-colors ${
                  hasActiveItem ? "text-foreground" : "text-muted-foreground hover:text-foreground"
                }`}
              >
                {isExpanded ? (
                  <ChevronDown className="w-4 h-4 shrink-0" />
                ) : (
                  <ChevronRight className="w-4 h-4 shrink-0" />
                )}
                {group.icon}
                <span className="flex-1 text-left">{group.label}</span>
              </button>

              {isExpanded && (
                <div className="ml-4 space-y-0.5">
                  {group.items.map((item) => {
                    const isActive = item.id === activeCategory;

                    return (
                      <button
                        key={item.id}
                        onClick={() => onCategoryChange(item.id)}
                        className={`w-full flex items-center gap-2 px-3 py-1.5 text-sm rounded-md transition-colors ${
                          isActive
                            ? `${getAccentColors("blue").bg} ${getAccentColors("blue").text}`
                            : "text-muted-foreground hover:text-foreground hover:bg-muted"
                        }`}
                      >
                        {item.icon}
                        <span className="flex-1 text-left">{item.label}</span>
                      </button>
                    );
                  })}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

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
          <ChevronDown className="w-4 h-4 text-muted-foreground shrink-0" />
        ) : (
          <ChevronRight className="w-4 h-4 text-muted-foreground shrink-0" />
        )}
        <span
          className={`px-2 py-0.5 text-xs font-medium rounded ${
            chunk.source === "claude"
              ? `${getAccentColors("blue").bg} ${getAccentColors("blue").text}`
              : chunk.source === "prompt"
                ? `${getAccentColors("green").bg} ${getAccentColors("green").text}`
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

const CHUNKS_PAGE_SIZE = 20;

function ConsolidatedAiOutputDisplay({
  chunks,
  totalEntries,
}: {
  chunks: AiOutputChunk[];
  totalEntries: number;
}) {
  const [visibleChunks, setVisibleChunks] = useState(CHUNKS_PAGE_SIZE);
  const LINE_HEIGHT_PX = 18;
  const HEADER_HEIGHT_PX = 45;
  const BASE_UI_HEIGHT_PX = 300;

  const displayedChunks = chunks.slice(0, visibleChunks);
  const totalLines = displayedChunks.reduce((acc, chunk) => acc + chunk.entry_count, 0);
  const estimatedTotalHeight =
    totalLines * LINE_HEIGHT_PX + displayedChunks.length * HEADER_HEIGHT_PX + BASE_UI_HEIGHT_PX;

  const shouldDefaultExpand = displayedChunks.length <= 1 || estimatedTotalHeight < window.innerHeight;

  return (
    <div className="space-y-4">
      <div className="text-xs text-muted-foreground mb-2">
        Showing {displayedChunks.length} of {chunks.length} chunks from {totalEntries} raw entries
      </div>
      {displayedChunks.map((chunk, i) => (
        <AiOutputChunkItem
          key={`chunk-${chunk.start_time}-${chunk.source}-${i}`}
          chunk={chunk}
          defaultExpanded={shouldDefaultExpand}
        />
      ))}
      {visibleChunks < chunks.length && (
        <button
          onClick={() => setVisibleChunks((prev) => prev + CHUNKS_PAGE_SIZE)}
          className="w-full py-2 text-xs text-muted-foreground hover:text-foreground flex items-center justify-center gap-1 border border-border rounded hover:bg-muted/50 transition-colors"
        >
          <ChevronDown className="w-3 h-3" />
          Show more chunks ({chunks.length - visibleChunks} remaining)
        </button>
      )}
    </div>
  );
}

// Note: Currently unused but kept for future use when filtering events by log type
function _getEventTypeForLogType(logType: JsonlLogType): string | undefined {
  switch (logType) {
    case "ai-output":
      return "ai_output";
    case "general":
      return "general";
    case "actions":
      return "action";
    case "image-recognition":
      return "image_recognition";
    default:
      return undefined;
  }
}

function EventsDisplay({ events }: { events: TaskRunEvent[] }) {
  const [expandedIds, setExpandedIds] = useState<Set<number>>(new Set());
  const [visibleCount, setVisibleCount] = useState(EVENTS_PAGE_SIZE);

  const loadMore = useCallback(() => {
    setVisibleCount((prev) => prev + EVENTS_PAGE_SIZE);
  }, []);

  const toggleExpanded = (id: number) => {
    setExpandedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  };

  return (
    <div className="space-y-2">
      {events.slice(0, visibleCount).map((event) => (
        <div key={event.id} className="border border-border rounded-lg overflow-hidden">
          <button
            onClick={() => toggleExpanded(event.id)}
            className="w-full flex items-center gap-2 px-3 py-2 bg-card hover:bg-muted/50 transition-colors text-left"
          >
            {expandedIds.has(event.id) ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground shrink-0" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground shrink-0" />
            )}
            <span
              className={`px-2 py-0.5 text-xs font-medium rounded ${
                event.event_type === "ai_output"
                  ? `${getAccentColors("blue").bg} ${getAccentColors("blue").text}`
                  : event.event_type === "action"
                    ? `${getAccentColors("green").bg} ${getAccentColors("green").text}`
                    : event.event_type === "image_recognition"
                      ? `${getAccentColors("purple").bg} ${getAccentColors("purple").text}`
                      : "bg-muted text-muted-foreground"
              }`}
            >
              {event.event_subtype || event.event_type}
            </span>
            <span className="text-xs text-muted-foreground">
              {formatTimestamp(event.timestamp)}
            </span>
            <span className="flex-1 text-sm truncate">{event.message}</span>
            {event.duration_ms && (
              <span className="text-xs text-muted-foreground">{event.duration_ms}ms</span>
            )}
          </button>
          {expandedIds.has(event.id) && (
            <div className="px-3 py-2 bg-muted/30 border-t border-border space-y-2">
              {event.workflow_name && (
                <div className="text-xs">
                  <span className="font-medium text-muted-foreground">Workflow:</span>{" "}
                  {event.workflow_name}
                </div>
              )}
              {event.state_name && (
                <div className="text-xs">
                  <span className="font-medium text-muted-foreground">State:</span>{" "}
                  {event.state_name}
                </div>
              )}
              {event.action_id && (
                <div className="text-xs">
                  <span className="font-medium text-muted-foreground">Action:</span>{" "}
                  {event.action_id}
                </div>
              )}
              {event.data && (
                <div>
                  <div className="text-xs font-medium text-muted-foreground mb-1">Data</div>
                  <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-48 overflow-y-auto">
                    {(() => {
                      try {
                        return JSON.stringify(JSON.parse(event.data), null, 2);
                      } catch {
                        return event.data;
                      }
                    })()}
                  </pre>
                </div>
              )}
            </div>
          )}
        </div>
      ))}
      {visibleCount < events.length && (
        <button
          onClick={loadMore}
          className="w-full py-2 text-xs text-muted-foreground hover:text-foreground flex items-center justify-center gap-1 border border-border rounded hover:bg-muted/50 transition-colors"
        >
          <ChevronDown className="w-3 h-3" />
          Show more ({events.length - visibleCount} remaining)
        </button>
      )}
    </div>
  );
}

function DevLogsSection() {
  const { selectedRunId, selectedRun } = useRunSelection();
  const [activeLogType, setActiveLogType] = useState<TextLogType>("backend");

  const { data: summary, isLoading: summaryLoading } = useTextLogsSummary(selectedRunId);
  const { data: logs, isLoading: logsLoading, error } = useTextLogs(activeLogType, selectedRunId);

  const logTypes: { type: TextLogType; label: string }[] = [
    { type: "backend", label: "Backend" },
    { type: "backend-err", label: "Backend Errors" },
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
      {selectedRun && (
        <div className="text-xs text-muted-foreground bg-muted/30 px-3 py-2 rounded-md">
          <span className="font-medium">Time range:</span> {formatTimestamp(selectedRun.created_at)}
          {selectedRun.completed_at
            ? ` → ${formatTimestamp(selectedRun.completed_at)}`
            : " → (still running)"}
        </div>
      )}

      <div className="flex gap-2 flex-wrap">
        {logTypes.map(({ type, label }) => (
          <button
            key={type}
            onClick={() => setActiveLogType(type)}
            className={`px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
              activeLogType === type
                ? `${getAccentColors("blue").bg} ${getAccentColors("blue").text} border ${getAccentColors("blue").border}`
                : "bg-muted text-muted-foreground hover:text-foreground"
            }`}
          >
            {label}
            {!summaryLoading && <span className="ml-1.5 opacity-60">({getLogCount(type)})</span>}
          </button>
        ))}
      </div>

      {logsLoading ? (
        <div className="flex items-center justify-center py-8 text-muted-foreground">
          <Loader2 className="w-5 h-5 animate-spin mr-2" />
          Loading logs...
        </div>
      ) : error ? (
        <div className={`flex items-center justify-center py-8 ${getStatusColors("error").text}`}>
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
      <div className={`flex items-center justify-center py-8 ${getStatusColors("error").text}`}>
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
        <div className="text-xs text-muted-foreground bg-muted/30 px-3 py-2 rounded-md space-y-1 shrink-0">
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
      <div className={`flex items-center justify-center py-8 ${getStatusColors("error").text}`}>
        <AlertCircle className="w-5 h-5 mr-2" />
        Error: {error.message}
      </div>
    );
  }

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
      {selectedRun && (
        <div className="text-xs text-muted-foreground bg-muted/30 px-3 py-2 rounded-md space-y-1 shrink-0">
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

      <div className="flex-1 min-h-0 border border-border rounded-lg overflow-hidden flex flex-col">
        <div className="flex-1 min-h-0 overflow-y-auto">
          <MarkdownViewer content={mainPrompt.content} className="min-h-full" />
        </div>
      </div>
    </div>
  );
}

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
      <div className={`flex items-center justify-center py-8 ${getStatusColors("error").text}`}>
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
      <div className="flex gap-2">
        <button
          onClick={() => setFilter("all")}
          className={`px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
            filter === "all"
              ? `${getAccentColors("blue").bg} ${getAccentColors("blue").text} border ${getAccentColors("blue").border}`
              : "bg-muted text-muted-foreground hover:text-foreground"
          }`}
        >
          All ({contextsData.contexts.length})
        </button>
        <button
          onClick={() => setFilter("user")}
          className={`px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
            filter === "user"
              ? `${getAccentColors("blue").bg} ${getAccentColors("blue").text} border ${getAccentColors("blue").border}`
              : "bg-muted text-muted-foreground hover:text-foreground"
          }`}
        >
          User ({userCount})
        </button>
        <button
          onClick={() => setFilter("builtin")}
          className={`px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
            filter === "builtin"
              ? `${getAccentColors("blue").bg} ${getAccentColors("blue").text} border ${getAccentColors("blue").border}`
              : "bg-muted text-muted-foreground hover:text-foreground"
          }`}
        >
          Built-in ({builtinCount})
        </button>
      </div>

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
                  ctx.context_type === "builtin"
                    ? getAccentColors("purple").text
                    : getAccentColors("green").text
                }`}
              />
              <span className="font-medium flex-1 truncate">{ctx.name}</span>
              {ctx.category && (
                <span className="px-2 py-0.5 text-xs rounded-full bg-muted">{ctx.category}</span>
              )}
              <span
                className={`px-2 py-0.5 text-xs rounded-full ${
                  ctx.context_type === "builtin"
                    ? `${getAccentColors("purple").bg} ${getAccentColors("purple").text}`
                    : `${getAccentColors("green").bg} ${getAccentColors("green").text}`
                }`}
              >
                {ctx.context_type}
              </span>
              <span
                className={`px-2 py-0.5 text-xs rounded-full ${
                  ctx.enabled
                    ? `${getStatusColors("success").bg} ${getStatusColors("success").text}`
                    : "bg-muted text-muted-foreground"
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

function ApiRequestsSection() {
  const { selectedRunId, selectedRun } = useRunSelection();
  const { data: logs, isLoading, error } = useJsonlLogsForTaskRun("api-requests", selectedRunId);
  const [expandedId, setExpandedId] = useState<string | null>(null);

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Globe className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view API requests</p>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin mr-2" />
        Loading API requests...
      </div>
    );
  }

  if (error) {
    return (
      <div className={`flex items-center justify-center py-8 ${getStatusColors("error").text}`}>
        <AlertCircle className="w-5 h-5 mr-2" />
        Error: {error.message}
      </div>
    );
  }

  if (!logs || logs.entries.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Globe className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">No API requests during this task run</p>
      </div>
    );
  }

  interface ApiRequestEntry {
    id: string;
    timestamp: string;
    step_name?: string;
    method: string;
    url: string;
    resolved_url?: string;
    status_code: number;
    status_text?: string;
    response_time_ms: number;
    response_body_type: string;
    response_body?: string;
    response_file_path?: string;
    response_size_bytes?: number;
    success: boolean;
    error?: string;
    extractions?: {
      variable_name: string;
      extracted_value?: string;
      success: boolean;
    }[];
    assertions?: {
      assertion_type: string;
      expected: string;
      actual: string;
      passed: boolean;
    }[];
  }

  return (
    <div className="space-y-4">
      {selectedRun && (
        <div className="text-xs text-muted-foreground bg-muted/30 px-3 py-2 rounded-md">
          <span className="font-medium">Time range:</span> {formatTimestamp(selectedRun.created_at)}
          {selectedRun.completed_at
            ? ` → ${formatTimestamp(selectedRun.completed_at)}`
            : " → (still running)"}
        </div>
      )}

      <div className="text-xs text-muted-foreground mb-2">
        Showing {logs.count} API requests during this task run
      </div>

      <div className="space-y-2">
        {logs.entries.map((entry, index) => {
          const req = entry as ApiRequestEntry;
          const entryKey = req.id || `api-req-${index}`;
          const isExpanded = expandedId === entryKey;

          return (
            <div key={entryKey} className="border border-border rounded-lg overflow-hidden">
              <button
                onClick={() => setExpandedId(isExpanded ? null : entryKey)}
                className="w-full flex items-center gap-3 px-4 py-3 bg-card hover:bg-muted/50 transition-colors text-left"
              >
                {isExpanded ? (
                  <ChevronDown className="w-4 h-4 text-muted-foreground" />
                ) : (
                  <ChevronRight className="w-4 h-4 text-muted-foreground" />
                )}
                <span
                  className={`px-2 py-0.5 text-xs font-medium rounded ${
                    req.method === "GET"
                      ? `${getAccentColors("green").bg} ${getAccentColors("green").text}`
                      : req.method === "POST"
                        ? `${getAccentColors("blue").bg} ${getAccentColors("blue").text}`
                        : req.method === "PUT" || req.method === "PATCH"
                          ? `${getAccentColors("yellow").bg} ${getAccentColors("yellow").text}`
                          : `${getAccentColors("red").bg} ${getAccentColors("red").text}`
                  }`}
                >
                  {req.method}
                </span>
                <span
                  className={`px-2 py-0.5 text-xs font-medium rounded ${
                    req.status_code >= 200 && req.status_code < 300
                      ? `${getStatusColors("success").bg} ${getStatusColors("success").text}`
                      : req.status_code >= 400
                        ? `${getStatusColors("error").bg} ${getStatusColors("error").text}`
                        : "bg-muted text-muted-foreground"
                  }`}
                >
                  {req.status_code}
                </span>
                <span className="flex-1 truncate text-sm font-mono">
                  {req.resolved_url || req.url}
                </span>
                <span className="text-xs text-muted-foreground">{req.response_time_ms}ms</span>
              </button>
              {isExpanded && (
                <div className="px-4 py-3 bg-muted/30 border-t border-border space-y-3">
                  {req.step_name && (
                    <div className="flex items-center gap-2">
                      <span className="text-xs font-medium text-muted-foreground">Step:</span>
                      <span className="text-xs">{req.step_name}</span>
                    </div>
                  )}
                  <div className="flex items-center gap-2">
                    <span className="text-xs font-medium text-muted-foreground">URL:</span>
                    <span className="text-xs font-mono break-all">
                      {req.resolved_url || req.url}
                    </span>
                  </div>
                  {req.response_body && (
                    <div>
                      <div className="text-xs font-medium text-muted-foreground mb-1">
                        Response Body ({req.response_body_type})
                      </div>
                      <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-48 overflow-y-auto">
                        {req.response_body_type === "json"
                          ? JSON.stringify(JSON.parse(req.response_body), null, 2)
                          : req.response_body.substring(0, 1000)}
                        {req.response_body.length > 1000 && "... (truncated)"}
                      </pre>
                    </div>
                  )}
                  {req.response_file_path && (
                    <div className="flex items-center gap-2">
                      <span className="text-xs font-medium text-muted-foreground">
                        Binary saved to:
                      </span>
                      <span className="text-xs font-mono">{req.response_file_path}</span>
                    </div>
                  )}
                  {req.extractions && req.extractions.length > 0 && (
                    <div>
                      <div className="text-xs font-medium text-muted-foreground mb-1">
                        Variable Extractions
                      </div>
                      <div className="space-y-1">
                        {req.extractions.map((ext, i) => (
                          <div
                            key={`reqext-${ext.variable_name}-${i}`}
                            className="flex items-center gap-2 text-xs"
                          >
                            {ext.success ? (
                              <CheckCircle
                                className={`w-3 h-3 ${getStatusColors("success").icon}`}
                              />
                            ) : (
                              <XCircle className={`w-3 h-3 ${getStatusColors("error").icon}`} />
                            )}
                            <span className="font-mono">{ext.variable_name}</span>
                            <span className="text-muted-foreground">=</span>
                            <span className="font-mono">{ext.extracted_value ?? "(failed)"}</span>
                          </div>
                        ))}
                      </div>
                    </div>
                  )}
                  {req.assertions && req.assertions.length > 0 && (
                    <div>
                      <div className="text-xs font-medium text-muted-foreground mb-1">
                        Assertions
                      </div>
                      <div className="space-y-1">
                        {req.assertions.map((a, i) => (
                          <div
                            key={`reqassert-${a.assertion_type}-${i}`}
                            className="flex items-center gap-2 text-xs"
                          >
                            {a.passed ? (
                              <CheckCircle
                                className={`w-3 h-3 ${getStatusColors("success").icon}`}
                              />
                            ) : (
                              <XCircle className={`w-3 h-3 ${getStatusColors("error").icon}`} />
                            )}
                            <span>{a.assertion_type}</span>
                            <span className="text-muted-foreground">expected:</span>
                            <span className="font-mono">{a.expected}</span>
                            <span className="text-muted-foreground">actual:</span>
                            <span className="font-mono">{a.actual}</span>
                          </div>
                        ))}
                      </div>
                    </div>
                  )}
                  {req.error && (
                    <div>
                      <div className={`text-xs font-medium ${getStatusColors("error").text} mb-1`}>
                        Error
                      </div>
                      <pre
                        className={`text-xs ${getStatusColors("error").bg} ${getStatusColors("error").text} p-2 rounded overflow-x-auto`}
                      >
                        {req.error}
                      </pre>
                    </div>
                  )}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

const AI_OUTPUT_EVENTS_PAGE_SIZE = 100;

function AiOutputSection() {
  const { selectedRunId, selectedRun } = useRunSelection();
  const isCompleted = selectedRun?.completed_at != null;
  const [aiOutputLimit, setAiOutputLimit] = useState(AI_OUTPUT_EVENTS_PAGE_SIZE);

  const {
    data: sqliteEvents,
    isLoading: sqliteLoading,
    error: sqliteError,
  } = useTaskRunEvents(isCompleted ? selectedRunId : null, "ai_output", aiOutputLimit);

  const {
    data: consolidatedOutput,
    isLoading: consolidatedLoading,
    error: consolidatedError,
  } = useConsolidatedAiOutput(!isCompleted ? selectedRunId : null);

  const isLoading = isCompleted ? sqliteLoading : consolidatedLoading;
  const error = isCompleted ? sqliteError : consolidatedError;

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <FileText className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view AI output</p>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin mr-2" />
        Loading AI output...
      </div>
    );
  }

  if (error) {
    return (
      <div className={`flex items-center justify-center py-8 ${getStatusColors("error").text}`}>
        <AlertCircle className="w-5 h-5 mr-2" />
        Error: {error.message}
      </div>
    );
  }

  if (isCompleted && sqliteEvents) {
    if (sqliteEvents.events.length === 0) {
      return (
        <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
          <FileText className="w-8 h-8 mb-3 opacity-50" />
          <p className="text-sm">No AI output for this task run</p>
        </div>
      );
    }
    return (
      <div className="space-y-4">
        <div className="text-xs text-muted-foreground bg-muted/30 px-3 py-2 rounded-md">
          <span className="px-2 py-0.5 rounded bg-muted">SQLite</span>
          <span className="ml-2">{sqliteEvents.count} entries</span>
        </div>
        <EventsDisplay events={sqliteEvents.events} />
        {sqliteEvents.events.length >= aiOutputLimit && (
          <button
            onClick={() => setAiOutputLimit((prev) => prev + AI_OUTPUT_EVENTS_PAGE_SIZE)}
            className="w-full py-2 text-xs text-muted-foreground hover:text-foreground flex items-center justify-center gap-1 border border-border rounded hover:bg-muted/50 transition-colors"
          >
            <ChevronDown className="w-3 h-3" />
            Load more AI output events
          </button>
        )}
      </div>
    );
  }

  if (consolidatedOutput && consolidatedOutput.chunks.length > 0) {
    return (
      <div className="space-y-4">
        <div className="text-xs text-muted-foreground bg-muted/30 px-3 py-2 rounded-md">
          <span className="px-2 py-0.5 rounded bg-muted">JSONL (real-time)</span>
        </div>
        <ConsolidatedAiOutputDisplay
          chunks={consolidatedOutput.chunks}
          totalEntries={consolidatedOutput.total_entries}
        />
      </div>
    );
  }

  return (
    <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
      <FileText className="w-8 h-8 mb-3 opacity-50" />
      <p className="text-sm">No AI output during this task run</p>
    </div>
  );
}

const EVENTS_SERVER_PAGE_SIZE = 200;

function EventsSection() {
  const { selectedRunId, selectedRun } = useRunSelection();
  const isCompleted = selectedRun?.completed_at != null;
  const [eventFilter, setEventFilter] = useState<string | undefined>(undefined);
  const [eventsLimit, setEventsLimit] = useState(EVENTS_SERVER_PAGE_SIZE);

  const {
    data: sqliteEvents,
    isLoading: sqliteLoading,
    error: sqliteError,
  } = useTaskRunEvents(isCompleted ? selectedRunId : null, eventFilter, eventsLimit);

  const { data: migratedSummary } = useTaskRunMigratedLogsSummary(
    isCompleted ? selectedRunId : null,
  );

  const {
    data: generalLogs,
    isLoading: generalLoading,
    error: generalError,
  } = useJsonlLogsForTaskRun("general", !isCompleted ? selectedRunId : null);
  const {
    data: _actionLogs,
    isLoading: actionLoading,
    error: actionError,
  } = useJsonlLogsForTaskRun(
    "actions",
    !isCompleted && eventFilter === "action" ? selectedRunId : null,
  );

  const isLoading = isCompleted ? sqliteLoading : generalLoading || actionLoading;
  const error = isCompleted ? sqliteError : generalError || actionError;

  const eventTypes = [
    {
      type: undefined,
      label: "All",
      count: migratedSummary?.events_count,
    },
    {
      type: "general",
      label: "General",
      count: migratedSummary?.events_by_type["general"],
    },
    {
      type: "action",
      label: "Actions",
      count: migratedSummary?.events_by_type["action"],
    },
  ];

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <List className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view events</p>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin mr-2" />
        Loading events...
      </div>
    );
  }

  if (error) {
    return (
      <div className={`flex items-center justify-center py-8 ${getStatusColors("error").text}`}>
        <AlertCircle className="w-5 h-5 mr-2" />
        Error: {error.message}
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {isCompleted && (
        <div className="flex gap-2 flex-wrap">
          {eventTypes.map(({ type, label, count }) => (
            <button
              key={label}
              onClick={() => { setEventFilter(type); setEventsLimit(EVENTS_SERVER_PAGE_SIZE); }}
              className={`px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
                eventFilter === type
                  ? `${getAccentColors("blue").bg} ${getAccentColors("blue").text} border ${getAccentColors("blue").border}`
                  : "bg-muted text-muted-foreground hover:text-foreground"
              }`}
            >
              {label}
              {count !== undefined && <span className="ml-1.5 opacity-60">({count})</span>}
            </button>
          ))}
        </div>
      )}

      {isCompleted && sqliteEvents ? (
        sqliteEvents.events.length > 0 ? (
          <div className="space-y-2">
            <div className="text-xs text-muted-foreground">
              Showing {sqliteEvents.count} events
              {migratedSummary?.events_count != null && sqliteEvents.count < migratedSummary.events_count && (
                <span className="ml-1">of {migratedSummary.events_count} total</span>
              )}
            </div>
            <EventsDisplay events={sqliteEvents.events} />
            {sqliteEvents.events.length >= eventsLimit && (
              <button
                onClick={() => setEventsLimit((prev) => prev + EVENTS_SERVER_PAGE_SIZE)}
                className="w-full py-2 text-xs text-muted-foreground hover:text-foreground flex items-center justify-center gap-1 border border-border rounded hover:bg-muted/50 transition-colors"
              >
                <ChevronDown className="w-3 h-3" />
                Load more events from server
              </button>
            )}
          </div>
        ) : (
          <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
            <List className="w-8 h-8 mb-3 opacity-50" />
            <p className="text-sm">No events for this task run</p>
          </div>
        )
      ) : generalLogs && generalLogs.entries.length > 0 ? (
        <div className="space-y-2">
          <div className="text-xs text-muted-foreground bg-muted/30 px-3 py-2 rounded-md">
            <span className="px-2 py-0.5 rounded bg-muted">JSONL (real-time)</span>
            <span className="ml-2">{generalLogs.count} entries</span>
          </div>
          <div className="border border-border rounded-lg overflow-hidden">
            <pre className="text-xs bg-background p-3 overflow-x-auto max-h-[500px] overflow-y-auto">
              {generalLogs.entries.map((entry, i) => (
                <div key={`genlog-${i}`} className="py-1 border-b border-border/50 last:border-0">
                  {JSON.stringify(entry, null, 2)}
                </div>
              ))}
            </pre>
          </div>
        </div>
      ) : (
        <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
          <List className="w-8 h-8 mb-3 opacity-50" />
          <p className="text-sm">No events during this task run</p>
        </div>
      )}
    </div>
  );
}

function ImageRecognitionSection() {
  const { selectedRunId, selectedRun } = useRunSelection();
  const isCompleted = selectedRun?.completed_at != null;

  const {
    data: sqliteEvents,
    isLoading: sqliteLoading,
    error: sqliteError,
  } = useTaskRunEvents(isCompleted ? selectedRunId : null, "image_recognition", 200);

  const {
    data: jsonlLogs,
    isLoading: jsonlLoading,
    error: jsonlError,
  } = useJsonlLogsForTaskRun("image-recognition", !isCompleted ? selectedRunId : null);

  const isLoading = isCompleted ? sqliteLoading : jsonlLoading;
  const error = isCompleted ? sqliteError : jsonlError;

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Eye className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view image recognition results</p>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin mr-2" />
        Loading image recognition results...
      </div>
    );
  }

  if (error) {
    return (
      <div className={`flex items-center justify-center py-8 ${getStatusColors("error").text}`}>
        <AlertCircle className="w-5 h-5 mr-2" />
        Error: {error.message}
      </div>
    );
  }

  if (isCompleted && sqliteEvents) {
    if (sqliteEvents.events.length === 0) {
      return (
        <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
          <Eye className="w-8 h-8 mb-3 opacity-50" />
          <p className="text-sm">No image recognition results for this task run</p>
        </div>
      );
    }
    return (
      <div className="space-y-4">
        <div className="text-xs text-muted-foreground bg-muted/30 px-3 py-2 rounded-md">
          <span className="px-2 py-0.5 rounded bg-muted">SQLite</span>
          <span className="ml-2">{sqliteEvents.count} results</span>
        </div>
        <EventsDisplay events={sqliteEvents.events} />
      </div>
    );
  }

  if (jsonlLogs && jsonlLogs.entries.length > 0) {
    return (
      <div className="space-y-4">
        <div className="text-xs text-muted-foreground bg-muted/30 px-3 py-2 rounded-md">
          <span className="px-2 py-0.5 rounded bg-muted">JSONL (real-time)</span>
          <span className="ml-2">{jsonlLogs.count} entries</span>
        </div>
        <div className="border border-border rounded-lg overflow-hidden">
          <pre className="text-xs bg-background p-3 overflow-x-auto max-h-[500px] overflow-y-auto">
            {jsonlLogs.entries.map((entry, i) => (
              <div key={`imglog-${i}`} className="py-1 border-b border-border/50 last:border-0">
                {JSON.stringify(entry, null, 2)}
              </div>
            ))}
          </pre>
        </div>
      </div>
    );
  }

  return (
    <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
      <Eye className="w-8 h-8 mb-3 opacity-50" />
      <p className="text-sm">No image recognition results during this task run</p>
    </div>
  );
}

function PlaywrightTestsSection() {
  const { selectedRunId, selectedRun } = useRunSelection();
  const isCompleted = selectedRun?.completed_at != null;

  const {
    data: jsonlLogs,
    isLoading: jsonlLoading,
    error: jsonlError,
  } = useJsonlLogsForTaskRun("playwright", !isCompleted ? selectedRunId : null);

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <TestTube className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view Playwright test results</p>
      </div>
    );
  }

  if (isCompleted) {
    return <PlaywrightResultsDisplay taskRunId={selectedRunId} />;
  }

  if (jsonlLoading) {
    return (
      <div className="flex items-center justify-center py-8 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin mr-2" />
        Loading Playwright results...
      </div>
    );
  }

  if (jsonlError) {
    return (
      <div className={`flex items-center justify-center py-8 ${getStatusColors("error").text}`}>
        <AlertCircle className="w-5 h-5 mr-2" />
        Error: {jsonlError.message}
      </div>
    );
  }

  if (jsonlLogs && jsonlLogs.entries.length > 0) {
    return (
      <div className="space-y-4">
        <div className="text-xs text-muted-foreground bg-muted/30 px-3 py-2 rounded-md">
          <span className="px-2 py-0.5 rounded bg-muted">JSONL (real-time)</span>
          <span className="ml-2">{jsonlLogs.count} entries</span>
        </div>
        <div className="border border-border rounded-lg overflow-hidden">
          <pre className="text-xs bg-background p-3 overflow-x-auto max-h-[500px] overflow-y-auto">
            {jsonlLogs.entries.map((entry, i) => (
              <div key={`pwlog-${i}`} className="py-1 border-b border-border/50 last:border-0">
                {JSON.stringify(entry, null, 2)}
              </div>
            ))}
          </pre>
        </div>
      </div>
    );
  }

  return (
    <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
      <TestTube className="w-8 h-8 mb-3 opacity-50" />
      <p className="text-sm">No Playwright test results during this task run</p>
    </div>
  );
}

function UnifiedApiRequestsSection() {
  const { selectedRunId, selectedRun } = useRunSelection();
  const isCompleted = selectedRun?.completed_at != null;

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Globe className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view API requests</p>
      </div>
    );
  }

  if (isCompleted) {
    return <ApiRequestsDisplay taskRunId={selectedRunId} />;
  }

  return <ApiRequestsSection />;
}

function McpCallsSection() {
  const { selectedRunId } = useRunSelection();

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Wifi className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view MCP calls</p>
      </div>
    );
  }

  return <McpCallsDisplay taskRunId={selectedRunId} />;
}

export function AiDataViewerTab() {
  const [activeCategory, setActiveCategory] = useState<DataCategory>("ai-prompt");
  const [selectedProcessSessionId, _setSelectedProcessSessionId] = useState<string | null>(null);

  const renderContent = () => {
    switch (activeCategory) {
      case "ai-prompt":
        return <AiPromptSection />;
      case "ai-output":
        return <AiOutputSection />;
      case "contexts":
        return <ContextsSection />;
      case "events":
        return <EventsSection />;
      case "api-requests":
        return <UnifiedApiRequestsSection />;
      case "mcp-calls":
        return <McpCallsSection />;
      case "image-recognition":
        return <ImageRecognitionSection />;
      case "playwright-tests":
        return <PlaywrightTestsSection />;
      case "awas-steps":
        return <AwasStepsSection />;
      case "execution-spans":
        return <ExecutionSpansSection />;
      case "dom-snapshots":
        return <DomSnapshotsPanel />;
      case "mobile-state":
        return <MobileStateSection />;
      case "mobile-screenshots":
        return <MobileScreenshotsSection />;
      case "mobile-logs":
        return <MobileLogsSection />;
      case "mobile-errors":
        return <MobileErrorsSection />;
      case "process-sessions":
        return <ProcessSessionsSection />;
      case "process-output":
        return <ProcessSessionOutputSection sessionId={selectedProcessSessionId} />;
      case "loaded-config":
        return <LoadedConfigSection />;
      case "dev-logs":
        return <DevLogsSection />;
      default:
        return null;
    }
  };

  const needsFlexLayout = ["ai-prompt", "loaded-config"].includes(activeCategory);

  return (
    <div className="h-full flex overflow-hidden">
      <SidebarNav activeCategory={activeCategory} onCategoryChange={setActiveCategory} />

      <div
        className={`flex-1 min-h-0 p-4 ${
          needsFlexLayout ? "overflow-hidden flex flex-col" : "overflow-auto"
        }`}
      >
        {renderContent()}
      </div>
    </div>
  );
}

export default AiDataViewerTab;
