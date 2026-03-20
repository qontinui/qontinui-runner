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

import { useState, useMemo, useEffect, Fragment } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
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
  Settings,
  MessageSquare,
  BookOpen,
  Code,
  Globe,
  SkipForward,
  Timer,
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
  Search,
} from "lucide-react";
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
  // SQLite migrated logs
  useTaskRunEvents,
  useTaskRunMigratedLogsSummary,
  useTaskRunPlaywrightResults,
  useTaskRunApiRequests,
  useTaskRunAwasSteps,
  useTaskRunMcpCalls,
  // Process sessions
  useProcessSessions,
  useProcessSessionOutput,
} from "../../hooks/useAiData";
import type {
  JsonlLogType,
  TextLogType,
  ContextInfo,
  AiOutputChunk,
  TaskRunEvent,
  TaskRunPlaywrightResultDb,
  TaskRunApiRequestDb,
  TaskRunAwasStepDb,
  ProcessSession,
} from "../../types/aiData";
import type { TaskRunMcpCallDb } from "../../types/mcp-config";
import { MarkdownViewer } from "../MarkdownViewer";
import { DomSnapshotsPanel } from "../dom-captures";
import { getStatusColors, getAccentColors } from "@/design-system";

// New flat category structure for sidebar navigation
type DataCategory =
  // AI Session group
  | "ai-prompt"
  | "ai-output"
  | "contexts"
  // Execution Logs group
  | "events"
  | "api-requests"
  | "mcp-calls"
  | "image-recognition"
  | "playwright-tests"
  | "awas-steps"
  | "execution-spans"
  // Captures group
  | "dom-snapshots"
  // Mobile group
  | "mobile-state"
  | "mobile-screenshots"
  | "mobile-logs"
  | "mobile-errors"
  // Processes group
  | "process-sessions"
  | "process-output"
  // Configuration group
  | "loaded-config"
  | "dev-logs";

// Navigation group definition
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

// Sidebar navigation component
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
        { id: "ai-prompt", label: "Prompt", icon: <MessageSquare className="w-4 h-4" /> },
        { id: "ai-output", label: "AI Output", icon: <FileText className="w-4 h-4" /> },
        { id: "contexts", label: "Contexts", icon: <BookOpen className="w-4 h-4" /> },
      ],
    },
    {
      id: "execution-logs",
      label: "Execution Logs",
      icon: <Cpu className="w-4 h-4" />,
      items: [
        { id: "events", label: "Events", icon: <List className="w-4 h-4" /> },
        { id: "api-requests", label: "API Requests", icon: <Globe className="w-4 h-4" /> },
        { id: "mcp-calls", label: "MCP Calls", icon: <Wifi className="w-4 h-4" /> },
        // Image Recognition - only shown in development mode (visual automation feature)
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
        // AWAS Steps - only shown in development mode
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
      items: [{ id: "dom-snapshots", label: "DOM Snapshots", icon: <Code className="w-4 h-4" /> }],
    },
    // Mobile group - only shown in development mode (unfinished feature)
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
        { id: "loaded-config", label: "Loaded Config", icon: <Settings className="w-4 h-4" /> },
        { id: "dev-logs", label: "Dev Logs", icon: <FileJson className="w-4 h-4" /> },
      ],
    },
  ];

  return (
    <div className="w-56 flex-shrink-0 border-r border-border bg-muted/30 overflow-y-auto">
      <div className="p-2 space-y-1">
        {navGroups.map((group) => {
          const isExpanded = expandedGroups.has(group.id);
          const hasActiveItem = group.items.some((item) => item.id === activeCategory);

          return (
            <div key={group.id}>
              {/* Group header */}
              <button
                onClick={() => toggleGroup(group.id)}
                className={`w-full flex items-center gap-2 px-3 py-2 text-sm font-medium rounded-md transition-colors ${
                  hasActiveItem ? "text-foreground" : "text-muted-foreground hover:text-foreground"
                }`}
              >
                {isExpanded ? (
                  <ChevronDown className="w-4 h-4 flex-shrink-0" />
                ) : (
                  <ChevronRight className="w-4 h-4 flex-shrink-0" />
                )}
                {group.icon}
                <span className="flex-1 text-left">{group.label}</span>
              </button>

              {/* Group items */}
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

function formatTimestamp(ts: string): string {
  try {
    return new Date(ts).toLocaleString();
  } catch {
    return ts;
  }
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
        <AiOutputChunkItem key={`chunk-${i}`} chunk={chunk} defaultExpanded={shouldDefaultExpand} />
      ))}
    </div>
  );
}

// Map UI log types to SQLite event_type values
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
      return undefined; // playwright and api-requests need special handling
  }
}

// Display SQLite events in a readable format
function EventsDisplay({ events }: { events: TaskRunEvent[] }) {
  const [expandedIds, setExpandedIds] = useState<Set<number>>(new Set());

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
      {events.map((event) => (
        <div key={event.id} className="border border-border rounded-lg overflow-hidden">
          <button
            onClick={() => toggleExpanded(event.id)}
            className="w-full flex items-center gap-2 px-3 py-2 bg-card hover:bg-muted/50 transition-colors text-left"
          >
            {expandedIds.has(event.id) ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground flex-shrink-0" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground flex-shrink-0" />
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
    </div>
  );
}

// Playwright Results Display - shows SQLite-stored test results
function PlaywrightResultsDisplay({ taskRunId }: { taskRunId: string }) {
  const { data: playwrightData, isLoading, error } = useTaskRunPlaywrightResults(taskRunId);
  const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());

  // Convert file paths to Tauri asset URLs for failure screenshots
  const getImageSrc = useMemo(() => {
    return (path: string) => {
      try {
        return convertFileSrc(path);
      } catch {
        return `file://${path}`;
      }
    };
  }, []);

  const toggleExpanded = (id: string) => {
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

  // Get status badge styling
  const getTestStatusStyle = (status: string) => {
    switch (status) {
      case "passed":
        return {
          bg: getStatusColors("success").bg,
          text: getStatusColors("success").text,
          icon: <CheckCircle className={`w-4 h-4 ${getStatusColors("success").icon}`} />,
        };
      case "failed":
        return {
          bg: getStatusColors("error").bg,
          text: getStatusColors("error").text,
          icon: <XCircle className={`w-4 h-4 ${getStatusColors("error").icon}`} />,
        };
      case "skipped":
        return {
          bg: "bg-muted",
          text: "text-muted-foreground",
          icon: <SkipForward className="w-4 h-4 text-muted-foreground" />,
        };
      case "timeout":
        return {
          bg: getAccentColors("yellow").bg,
          text: getAccentColors("yellow").text,
          icon: <Timer className={`w-4 h-4 ${getAccentColors("yellow").text}`} />,
        };
      default:
        return {
          bg: "bg-muted",
          text: "text-muted-foreground",
          icon: <Clock className="w-4 h-4 text-muted-foreground" />,
        };
    }
  };

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin mr-2" />
        Loading Playwright results...
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

  if (!playwrightData || playwrightData.results.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <FileJson className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">No Playwright test results for this task run</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {/* Summary stats */}
      <div className="flex items-center gap-4 px-3 py-2 bg-muted/30 rounded-md">
        <span data-content-role="label" className="text-sm font-medium">
          Test Results:
        </span>
        <span
          data-content-role="metric"
          data-content-label="tests passed"
          className={`flex items-center gap-1 text-sm ${getStatusColors("success").text}`}
        >
          <CheckCircle className="w-4 h-4" />
          {playwrightData.passed} passed
        </span>
        <span
          data-content-role="metric"
          data-content-label="tests failed"
          className={`flex items-center gap-1 text-sm ${getStatusColors("error").text}`}
        >
          <XCircle className="w-4 h-4" />
          {playwrightData.failed} failed
        </span>
        <span
          data-content-role="metric"
          data-content-label="total tests"
          className="text-xs text-muted-foreground ml-auto"
        >
          {playwrightData.count} total tests
        </span>
      </div>

      {/* Test results list */}
      <div className="space-y-2">
        {playwrightData.results.map((result: TaskRunPlaywrightResultDb) => {
          const statusStyle = getTestStatusStyle(result.status);
          const isExpanded = expandedIds.has(result.id);
          const hasExpandableContent =
            result.error_message ||
            result.console_output ||
            result.stdout ||
            result.stderr ||
            result.failure_screenshot_path ||
            result.page_snapshot;

          return (
            <div key={result.id} className="border border-border rounded-lg overflow-hidden">
              <button
                onClick={() => hasExpandableContent && toggleExpanded(result.id)}
                className={`w-full flex items-center gap-3 px-4 py-3 bg-card transition-colors text-left ${
                  hasExpandableContent ? "hover:bg-muted/50 cursor-pointer" : "cursor-default"
                }`}
              >
                {hasExpandableContent ? (
                  isExpanded ? (
                    <ChevronDown className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                  ) : (
                    <ChevronRight className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                  )
                ) : (
                  <div className="w-4 h-4 flex-shrink-0" />
                )}
                {statusStyle.icon}
                <span className="font-medium flex-1 truncate">{result.test_name}</span>
                <span
                  className={`px-2 py-0.5 text-xs font-medium rounded ${statusStyle.bg} ${statusStyle.text}`}
                >
                  {result.status}
                </span>
                {result.duration_ms !== null && result.duration_ms !== undefined && (
                  <span className="text-xs text-muted-foreground">
                    {(result.duration_ms / 1000).toFixed(2)}s
                  </span>
                )}
              </button>
              {isExpanded && hasExpandableContent && (
                <div className="px-4 py-3 bg-muted/30 border-t border-border space-y-3">
                  {result.spec_file && (
                    <div className="text-xs">
                      <span className="font-medium text-muted-foreground">Spec file:</span>{" "}
                      <span className="font-mono">{result.spec_file}</span>
                    </div>
                  )}
                  {(result.assertions_passed > 0 || result.assertions_failed > 0) && (
                    <div className="text-xs">
                      <span className="font-medium text-muted-foreground">Assertions:</span>{" "}
                      <span className={getStatusColors("success").text}>
                        {result.assertions_passed} passed
                      </span>
                      {result.assertions_failed > 0 && (
                        <>
                          {" / "}
                          <span className={getStatusColors("error").text}>
                            {result.assertions_failed} failed
                          </span>
                        </>
                      )}
                    </div>
                  )}
                  {result.error_message && (
                    <div>
                      <div className={`text-xs font-medium ${getStatusColors("error").text} mb-1`}>
                        Error Message
                      </div>
                      <pre
                        className={`text-xs ${getStatusColors("error").bg} ${getStatusColors("error").text} p-2 rounded overflow-x-auto max-h-48 overflow-y-auto whitespace-pre-wrap`}
                      >
                        {result.error_message}
                      </pre>
                    </div>
                  )}
                  {result.console_output && (
                    <div>
                      <div className="text-xs font-medium text-muted-foreground mb-1">
                        Console Output
                      </div>
                      <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-48 overflow-y-auto whitespace-pre-wrap">
                        {result.console_output}
                      </pre>
                    </div>
                  )}
                  {result.stdout && (
                    <div>
                      <div className="text-xs font-medium text-muted-foreground mb-1">
                        Standard Output
                      </div>
                      <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-32 overflow-y-auto whitespace-pre-wrap">
                        {result.stdout}
                      </pre>
                    </div>
                  )}
                  {result.stderr && (
                    <div>
                      <div className={`text-xs font-medium ${getAccentColors("yellow").text} mb-1`}>
                        Standard Error
                      </div>
                      <pre
                        className={`text-xs ${getAccentColors("yellow").bg} p-2 rounded overflow-x-auto max-h-32 overflow-y-auto whitespace-pre-wrap`}
                      >
                        {result.stderr}
                      </pre>
                    </div>
                  )}
                  {result.page_snapshot && (
                    <div>
                      <div className="text-xs font-medium text-muted-foreground mb-1">
                        Page Snapshot
                      </div>
                      <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-48 overflow-y-auto whitespace-pre-wrap">
                        {result.page_snapshot}
                      </pre>
                    </div>
                  )}
                  {result.failure_screenshot_path && (
                    <div>
                      <div className="text-xs font-medium text-muted-foreground mb-1">
                        Failure Screenshot
                      </div>
                      <div className="text-xs text-muted-foreground mb-2 font-mono truncate">
                        {result.failure_screenshot_path}
                      </div>
                      <img
                        src={getImageSrc(result.failure_screenshot_path)}
                        alt="Failure screenshot"
                        className="max-w-full max-h-64 object-contain rounded border border-border"
                        onError={(e) => {
                          (e.target as HTMLImageElement).style.display = "none";
                        }}
                      />
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

// API Requests Display - shows SQLite-stored API request results
function ApiRequestsDisplay({ taskRunId }: { taskRunId: string }) {
  const { data: apiData, isLoading, error } = useTaskRunApiRequests(taskRunId);
  const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());

  const toggleExpanded = (id: string) => {
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

  // Get method badge styling
  const getMethodStyle = (method: string) => {
    switch (method.toUpperCase()) {
      case "GET":
        return {
          bg: getAccentColors("green").bg,
          text: getAccentColors("green").text,
        };
      case "POST":
        return {
          bg: getAccentColors("blue").bg,
          text: getAccentColors("blue").text,
        };
      case "PUT":
      case "PATCH":
        return {
          bg: getAccentColors("yellow").bg,
          text: getAccentColors("yellow").text,
        };
      case "DELETE":
        return {
          bg: getAccentColors("red").bg,
          text: getAccentColors("red").text,
        };
      default:
        return { bg: "bg-muted", text: "text-muted-foreground" };
    }
  };

  // Get status code badge styling
  const getStatusCodeStyle = (statusCode: number) => {
    if (statusCode >= 200 && statusCode < 300) {
      return {
        bg: getStatusColors("success").bg,
        text: getStatusColors("success").text,
      };
    } else if (statusCode >= 400 && statusCode < 500) {
      return {
        bg: getAccentColors("yellow").bg,
        text: getAccentColors("yellow").text,
      };
    } else if (statusCode >= 500) {
      return {
        bg: getStatusColors("error").bg,
        text: getStatusColors("error").text,
      };
    }
    return { bg: "bg-muted", text: "text-muted-foreground" };
  };

  // Truncate URL for display
  const truncateUrl = (url: string, maxLength: number = 60) => {
    if (url.length <= maxLength) return url;
    return url.substring(0, maxLength - 3) + "...";
  };

  // Parse JSON safely
  const parseJson = (jsonStr: string | null | undefined): unknown => {
    if (!jsonStr) return null;
    try {
      return JSON.parse(jsonStr);
    } catch {
      return jsonStr;
    }
  };

  // Format JSON for display
  const formatJson = (data: unknown): string => {
    if (data === null || data === undefined) return "";
    if (typeof data === "string") return data;
    try {
      return JSON.stringify(data, null, 2);
    } catch {
      return String(data);
    }
  };

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

  if (!apiData || apiData.requests.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Globe className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">No API requests for this task run</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {/* Summary stats */}
      <div className="flex items-center gap-4 px-3 py-2 bg-muted/30 rounded-md">
        <span data-content-role="label" className="text-sm font-medium">
          API Requests:
        </span>
        <span
          data-content-role="metric"
          data-content-label="API requests successful"
          className={`flex items-center gap-1 text-sm ${getStatusColors("success").text}`}
        >
          <CheckCircle className="w-4 h-4" />
          {apiData.success_count} successful
        </span>
        <span
          data-content-role="metric"
          data-content-label="API requests failed"
          className={`flex items-center gap-1 text-sm ${getStatusColors("error").text}`}
        >
          <XCircle className="w-4 h-4" />
          {apiData.failed_count} failed
        </span>
        <span
          data-content-role="metric"
          data-content-label="total API requests"
          className="text-xs text-muted-foreground ml-auto"
        >
          {apiData.count} total requests
        </span>
      </div>

      {/* Request list */}
      <div className="space-y-2">
        {apiData.requests.map((request: TaskRunApiRequestDb) => {
          const methodStyle = getMethodStyle(request.method);
          const statusStyle = getStatusCodeStyle(request.status_code);
          const isExpanded = expandedIds.has(request.id);

          // Parse JSON fields
          const requestHeaders = parseJson(request.request_headers);
          const responseHeaders = parseJson(request.response_headers);
          const extractions = parseJson(request.extractions) as Array<{
            variable_name: string;
            extracted_value?: string;
            success: boolean;
          }> | null;
          const assertions = parseJson(request.assertions) as Array<{
            type: string;
            expected: string;
            actual: string;
            passed: boolean;
          }> | null;

          const hasExpandableContent =
            request.resolved_url !== request.url ||
            requestHeaders ||
            request.request_body ||
            responseHeaders ||
            request.response_body ||
            extractions ||
            assertions ||
            request.error_message;

          return (
            <div key={request.id} className="border border-border rounded-lg overflow-hidden">
              <button
                onClick={() => hasExpandableContent && toggleExpanded(request.id)}
                className={`w-full flex items-center gap-3 px-4 py-3 bg-card transition-colors text-left ${
                  hasExpandableContent ? "hover:bg-muted/50 cursor-pointer" : "cursor-default"
                }`}
              >
                {hasExpandableContent ? (
                  isExpanded ? (
                    <ChevronDown className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                  ) : (
                    <ChevronRight className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                  )
                ) : (
                  <div className="w-4 h-4 flex-shrink-0" />
                )}
                <span
                  className={`px-2 py-0.5 text-xs font-medium rounded ${methodStyle.bg} ${methodStyle.text}`}
                >
                  {request.method}
                </span>
                <span
                  className={`px-2 py-0.5 text-xs font-medium rounded ${statusStyle.bg} ${statusStyle.text}`}
                >
                  {request.status_code}
                </span>
                <span className="flex-1 truncate text-sm font-mono">
                  {truncateUrl(request.resolved_url || request.url)}
                </span>
                <span className="text-xs text-muted-foreground">{request.response_time_ms}ms</span>
              </button>
              {isExpanded && hasExpandableContent && (
                <div className="px-4 py-3 bg-muted/30 border-t border-border space-y-3">
                  {request.step_name && (
                    <div className="text-xs">
                      <span className="font-medium text-muted-foreground">Step:</span>{" "}
                      {request.step_name}
                    </div>
                  )}
                  <div className="text-xs">
                    <span className="font-medium text-muted-foreground">Full URL:</span>{" "}
                    <span className="font-mono break-all">
                      {request.resolved_url || request.url}
                    </span>
                  </div>
                  {requestHeaders != null && Object.keys(requestHeaders as object).length > 0 && (
                    <CollapsibleSection title="Request Headers">
                      <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-32 overflow-y-auto">
                        {formatJson(requestHeaders)}
                      </pre>
                    </CollapsibleSection>
                  )}
                  {request.request_body && (
                    <CollapsibleSection title="Request Body">
                      <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-48 overflow-y-auto whitespace-pre-wrap">
                        {(() => {
                          try {
                            return JSON.stringify(JSON.parse(request.request_body), null, 2);
                          } catch {
                            return request.request_body;
                          }
                        })()}
                      </pre>
                    </CollapsibleSection>
                  )}
                  {responseHeaders != null && Object.keys(responseHeaders as object).length > 0 && (
                    <CollapsibleSection title="Response Headers">
                      <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-32 overflow-y-auto">
                        {formatJson(responseHeaders)}
                      </pre>
                    </CollapsibleSection>
                  )}
                  {request.response_body && (
                    <CollapsibleSection title={`Response Body (${request.response_body_type})`}>
                      <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-48 overflow-y-auto whitespace-pre-wrap">
                        {(() => {
                          try {
                            if (request.response_body_type === "json") {
                              return JSON.stringify(JSON.parse(request.response_body), null, 2);
                            }
                            const truncated = request.response_body.substring(0, 2000);
                            return (
                              truncated +
                              (request.response_body.length > 2000 ? "\n... (truncated)" : "")
                            );
                          } catch {
                            const truncated = request.response_body.substring(0, 2000);
                            return (
                              truncated +
                              (request.response_body.length > 2000 ? "\n... (truncated)" : "")
                            );
                          }
                        })()}
                      </pre>
                    </CollapsibleSection>
                  )}
                  {extractions && extractions.length > 0 && (
                    <div>
                      <div className="text-xs font-medium text-muted-foreground mb-1">
                        Extractions ({extractions.length})
                      </div>
                      <div className="space-y-1">
                        {extractions.map((ext, i) => (
                          <div
                            key={`ext-${ext.variable_name}-${i}`}
                            className="flex items-center gap-2 text-xs"
                          >
                            {ext.success ? (
                              <CheckCircle
                                className={`w-3 h-3 ${getStatusColors("success").icon}`}
                              />
                            ) : (
                              <XCircle className={`w-3 h-3 ${getStatusColors("error").icon}`} />
                            )}
                            <span className="font-mono font-medium">{ext.variable_name}</span>
                            <span className="text-muted-foreground">=</span>
                            <span className="font-mono truncate max-w-xs">
                              {ext.extracted_value ?? "(failed)"}
                            </span>
                          </div>
                        ))}
                      </div>
                    </div>
                  )}
                  {assertions && assertions.length > 0 && (
                    <div>
                      <div className="text-xs font-medium text-muted-foreground mb-1">
                        Assertions ({assertions.length})
                      </div>
                      <div className="space-y-1">
                        {assertions.map((a, i) => (
                          <div
                            key={`assert-${a.type}-${i}`}
                            className="flex items-center gap-2 text-xs flex-wrap"
                          >
                            {a.passed ? (
                              <CheckCircle
                                className={`w-3 h-3 ${getStatusColors("success").icon}`}
                              />
                            ) : (
                              <XCircle className={`w-3 h-3 ${getStatusColors("error").icon}`} />
                            )}
                            <span className="font-medium">{a.type}</span>
                            <span className="text-muted-foreground">expected:</span>
                            <span className="font-mono">{a.expected}</span>
                            <span className="text-muted-foreground">actual:</span>
                            <span className="font-mono">{a.actual}</span>
                          </div>
                        ))}
                      </div>
                    </div>
                  )}
                  {request.error_message && (
                    <div>
                      <div className={`text-xs font-medium ${getStatusColors("error").text} mb-1`}>
                        Error
                      </div>
                      <pre
                        className={`text-xs ${getStatusColors("error").bg} ${getStatusColors("error").text} p-2 rounded overflow-x-auto max-h-32 overflow-y-auto whitespace-pre-wrap`}
                      >
                        {request.error_message}
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

// Collapsible section helper for API request details
function CollapsibleSection({
  title,
  children,
  defaultOpen = false,
}: {
  title: string;
  children: React.ReactNode;
  defaultOpen?: boolean;
}) {
  const [isOpen, setIsOpen] = useState(defaultOpen);

  return (
    <div>
      <button
        onClick={() => setIsOpen(!isOpen)}
        className="flex items-center gap-1 text-xs font-medium text-muted-foreground hover:text-foreground transition-colors mb-1"
      >
        {isOpen ? <ChevronDown className="w-3 h-3" /> : <ChevronRight className="w-3 h-3" />}
        {title}
      </button>
      {isOpen && children}
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
                ? `${getAccentColors("blue").bg} ${getAccentColors("blue").text} border ${getAccentColors("blue").border}`
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
      <div className={`flex items-center justify-center py-8 ${getStatusColors("error").text}`}>
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
      {/* Filter tabs */}
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

// API Requests Section - dedicated view for API request/response data
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

  // Type for API request log entries
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
    extractions?: { variable_name: string; extracted_value?: string; success: boolean }[];
    assertions?: { assertion_type: string; expected: string; actual: string; passed: boolean }[];
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

      <div className="text-xs text-muted-foreground mb-2">
        Showing {logs.count} API requests during this task run
      </div>

      <div className="space-y-2">
        {logs.entries.map((entry, index) => {
          const req = entry as ApiRequestEntry;
          const isExpanded = expandedId === (req.id || index.toString());

          return (
            <div key={req.id || index} className="border border-border rounded-lg overflow-hidden">
              <button
                onClick={() => setExpandedId(isExpanded ? null : req.id || index.toString())}
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

// AI Output Section - shows consolidated AI output (uses SQLite for completed, JSONL for running)
function AiOutputSection() {
  const { selectedRunId, selectedRun } = useRunSelection();
  const isCompleted = selectedRun?.completed_at != null;

  // SQLite query for completed tasks
  const {
    data: sqliteEvents,
    isLoading: sqliteLoading,
    error: sqliteError,
  } = useTaskRunEvents(isCompleted ? selectedRunId : null, "ai_output");

  // JSONL query for running tasks (real-time)
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

  // Show SQLite events for completed tasks
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
      </div>
    );
  }

  // Show consolidated output for running tasks
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

// Events Section - shows general and action events
function EventsSection() {
  const { selectedRunId, selectedRun } = useRunSelection();
  const isCompleted = selectedRun?.completed_at != null;
  const [eventFilter, setEventFilter] = useState<string | undefined>(undefined);

  // SQLite query for completed tasks
  const {
    data: sqliteEvents,
    isLoading: sqliteLoading,
    error: sqliteError,
  } = useTaskRunEvents(isCompleted ? selectedRunId : null, eventFilter);

  // Summary for counts
  const { data: migratedSummary } = useTaskRunMigratedLogsSummary(
    isCompleted ? selectedRunId : null,
  );

  // JSONL queries for running tasks
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
    { type: undefined, label: "All", count: migratedSummary?.events_count },
    { type: "general", label: "General", count: migratedSummary?.events_by_type["general"] },
    { type: "action", label: "Actions", count: migratedSummary?.events_by_type["action"] },
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
      {/* Filter buttons */}
      {isCompleted && (
        <div className="flex gap-2 flex-wrap">
          {eventTypes.map(({ type, label, count }) => (
            <button
              key={label}
              onClick={() => setEventFilter(type)}
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

      {/* Events content */}
      {isCompleted && sqliteEvents ? (
        sqliteEvents.events.length > 0 ? (
          <div className="space-y-2">
            <div className="text-xs text-muted-foreground">Showing {sqliteEvents.count} events</div>
            <EventsDisplay events={sqliteEvents.events} />
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

// Image Recognition Section
function ImageRecognitionSection() {
  const { selectedRunId, selectedRun } = useRunSelection();
  const isCompleted = selectedRun?.completed_at != null;

  // SQLite query for completed tasks
  const {
    data: sqliteEvents,
    isLoading: sqliteLoading,
    error: sqliteError,
  } = useTaskRunEvents(isCompleted ? selectedRunId : null, "image_recognition");

  // JSONL query for running tasks
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

  // Show SQLite events for completed tasks
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

  // Show JSONL logs for running tasks
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

// Playwright Tests Section
function PlaywrightTestsSection() {
  const { selectedRunId, selectedRun } = useRunSelection();
  const isCompleted = selectedRun?.completed_at != null;

  // JSONL query for running tasks
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

  // For completed tasks, use the PlaywrightResultsDisplay which queries SQLite
  if (isCompleted) {
    return <PlaywrightResultsDisplay taskRunId={selectedRunId} />;
  }

  // For running tasks, show JSONL logs
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

// AWAS Steps Section - displays AWAS step execution results from SQLite
function AwasStepsSection() {
  const { selectedRunId, selectedRun: _selectedRun } = useRunSelection();
  const { data: awasData, isLoading, error } = useTaskRunAwasSteps(selectedRunId);
  const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());

  const toggleExpanded = (id: string) => {
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

  // Get step type badge styling
  const getStepTypeStyle = (stepType: string) => {
    switch (stepType) {
      case "awas_discover":
        return {
          bg: getAccentColors("blue").bg,
          text: getAccentColors("blue").text,
          label: "Discover",
        };
      case "awas_execute":
        return {
          bg: getAccentColors("green").bg,
          text: getAccentColors("green").text,
          label: "Execute",
        };
      case "awas_check_support":
        return {
          bg: getAccentColors("purple").bg,
          text: getAccentColors("purple").text,
          label: "Check Support",
        };
      case "awas_list_actions":
        return {
          bg: getAccentColors("yellow").bg,
          text: getAccentColors("yellow").text,
          label: "List Actions",
        };
      case "awas_extract_elements":
        return {
          bg: "bg-muted",
          text: "text-foreground",
          label: "Extract Elements",
        };
      default:
        return {
          bg: "bg-muted",
          text: "text-muted-foreground",
          label: stepType.replace("awas_", "").replace(/_/g, " "),
        };
    }
  };

  // Parse JSON safely
  const parseJson = (jsonStr: string | null | undefined): unknown => {
    if (!jsonStr) return null;
    try {
      return JSON.parse(jsonStr);
    } catch {
      return jsonStr;
    }
  };

  // Format JSON for display
  const formatJson = (data: unknown): string => {
    if (data === null || data === undefined) return "";
    if (typeof data === "string") return data;
    try {
      return JSON.stringify(data, null, 2);
    } catch {
      return String(data);
    }
  };

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Bot className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view AWAS steps</p>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin mr-2" />
        Loading AWAS steps...
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

  if (!awasData || awasData.steps.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Bot className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">No AWAS steps for this task run</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {/* Summary stats */}
      <div className="flex items-center gap-4 px-3 py-2 bg-muted/30 rounded-md">
        <span className="text-sm font-medium">AWAS Steps:</span>
        <span className={`flex items-center gap-1 text-sm ${getStatusColors("success").text}`}>
          <CheckCircle className="w-4 h-4" />
          {awasData.success_count} successful
        </span>
        <span className={`flex items-center gap-1 text-sm ${getStatusColors("error").text}`}>
          <XCircle className="w-4 h-4" />
          {awasData.failed_count} failed
        </span>
        <span className="text-xs text-muted-foreground ml-auto">{awasData.count} total steps</span>
      </div>

      {/* Steps list */}
      <div className="space-y-2">
        {awasData.steps.map((step: TaskRunAwasStepDb) => {
          const stepTypeStyle = getStepTypeStyle(step.step_type);
          const isExpanded = expandedIds.has(step.id);
          const parameters = parseJson(step.parameters);
          const responseData = parseJson(step.response_data);
          const hasExpandableContent =
            step.url || step.action_id || parameters || responseData || step.error_message;

          return (
            <div key={step.id} className="border border-border rounded-lg overflow-hidden">
              <button
                onClick={() => hasExpandableContent && toggleExpanded(step.id)}
                className={`w-full flex items-center gap-3 px-4 py-3 bg-card transition-colors text-left ${
                  hasExpandableContent ? "hover:bg-muted/50 cursor-pointer" : "cursor-default"
                }`}
              >
                {hasExpandableContent ? (
                  isExpanded ? (
                    <ChevronDown className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                  ) : (
                    <ChevronRight className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                  )
                ) : (
                  <div className="w-4 h-4 flex-shrink-0" />
                )}
                {step.success ? (
                  <CheckCircle className={`w-4 h-4 ${getStatusColors("success").icon}`} />
                ) : (
                  <XCircle className={`w-4 h-4 ${getStatusColors("error").icon}`} />
                )}
                <span
                  className={`px-2 py-0.5 text-xs font-medium rounded ${stepTypeStyle.bg} ${stepTypeStyle.text}`}
                >
                  {stepTypeStyle.label}
                </span>
                <span className="flex-1 truncate text-sm">
                  {step.step_name || step.action_id || step.url || "Step"}
                </span>
                {step.duration_ms !== null && step.duration_ms !== undefined && (
                  <span className="text-xs text-muted-foreground">{step.duration_ms}ms</span>
                )}
              </button>
              {isExpanded && hasExpandableContent && (
                <div className="px-4 py-3 bg-muted/30 border-t border-border space-y-3">
                  {step.step_name && (
                    <div className="text-xs">
                      <span className="font-medium text-muted-foreground">Step Name:</span>{" "}
                      {step.step_name}
                    </div>
                  )}
                  {step.url && (
                    <div className="text-xs">
                      <span className="font-medium text-muted-foreground">URL:</span>{" "}
                      <span className="font-mono break-all">{step.url}</span>
                    </div>
                  )}
                  {step.action_id && (
                    <div className="text-xs">
                      <span className="font-medium text-muted-foreground">Action ID:</span>{" "}
                      <span className="font-mono">{step.action_id}</span>
                    </div>
                  )}
                  {parameters != null && (
                    <CollapsibleSection title="Parameters">
                      <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-48 overflow-y-auto whitespace-pre-wrap">
                        {formatJson(parameters)}
                      </pre>
                    </CollapsibleSection>
                  )}
                  {responseData != null && (
                    <CollapsibleSection title="Response Data" defaultOpen={!step.error_message}>
                      <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-64 overflow-y-auto whitespace-pre-wrap">
                        {formatJson(responseData)}
                      </pre>
                    </CollapsibleSection>
                  )}
                  {step.error_message && (
                    <div>
                      <div className={`text-xs font-medium ${getStatusColors("error").text} mb-1`}>
                        Error
                      </div>
                      <pre
                        className={`text-xs ${getStatusColors("error").bg} ${getStatusColors("error").text} p-2 rounded overflow-x-auto max-h-32 overflow-y-auto whitespace-pre-wrap`}
                      >
                        {step.error_message}
                      </pre>
                    </div>
                  )}
                  <div className="text-xs text-muted-foreground">
                    Created: {formatTimestamp(step.created_at)}
                  </div>
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

// Unified API Requests Section - uses SQLite for completed, JSONL for running
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

  // For completed tasks, use the ApiRequestsDisplay which queries SQLite
  if (isCompleted) {
    return <ApiRequestsDisplay taskRunId={selectedRunId} />;
  }

  // For running tasks, use the ApiRequestsSection which reads JSONL
  return <ApiRequestsSection />;
}

// =============================================================================
// MCP Calls Section
// =============================================================================

// MCP Calls Display - shows SQLite-stored MCP call results
function McpCallsDisplay({ taskRunId }: { taskRunId: string }) {
  const { data: mcpData, isLoading, error } = useTaskRunMcpCalls(taskRunId);
  const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());

  const toggleExpanded = (id: string) => {
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

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <AlertCircle className="w-8 h-8 mb-3 text-destructive opacity-50" />
        <p className="text-sm">Error loading MCP calls</p>
      </div>
    );
  }

  if (!mcpData || mcpData.calls.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Wifi className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">No MCP calls for this task run</p>
      </div>
    );
  }

  // Parse JSON fields safely
  const parseJson = (jsonStr: string | null | undefined): unknown => {
    if (!jsonStr) return null;
    try {
      return JSON.parse(jsonStr);
    } catch {
      return jsonStr;
    }
  };

  return (
    <div className="space-y-4">
      {/* Summary stats */}
      <div className="flex gap-4 text-xs text-muted-foreground">
        <span>Total: {mcpData.count}</span>
        <span className="text-green-400">Success: {mcpData.success_count}</span>
        <span className="text-red-400">Failed: {mcpData.failed_count}</span>
      </div>

      {/* MCP calls list */}
      <div className="space-y-3">
        {mcpData.calls.map((call: TaskRunMcpCallDb) => {
          const isExpanded = expandedIds.has(call.id);
          const response = parseJson(call.response);
          const args = parseJson(call.arguments);
          const resolvedArgs = parseJson(call.resolved_arguments);

          return (
            <div key={call.id} className="border border-border rounded-lg overflow-hidden bg-card">
              {/* Header row - clickable to expand */}
              <button
                onClick={() => toggleExpanded(call.id)}
                className="w-full flex items-center gap-3 p-3 hover:bg-muted/50 transition-colors text-left"
              >
                {/* Status indicator */}
                <div
                  className={`w-2 h-2 rounded-full flex-shrink-0 ${
                    call.success ? "bg-green-500" : "bg-red-500"
                  }`}
                />

                {/* Server and tool info */}
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="px-2 py-0.5 text-xs font-medium rounded bg-purple-500/20 text-purple-400">
                      {call.server_name || call.server_id}
                    </span>
                    <span className="text-sm font-mono text-foreground truncate">
                      {call.tool_name}
                    </span>
                  </div>
                  {call.step_name && (
                    <p className="text-xs text-muted-foreground mt-1 truncate">
                      Step: {call.step_name}
                    </p>
                  )}
                </div>

                {/* Duration */}
                <span className="text-xs text-muted-foreground flex-shrink-0">
                  {call.duration_ms}ms
                </span>

                {/* Expand chevron */}
                {isExpanded ? (
                  <ChevronDown className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                ) : (
                  <ChevronRight className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                )}
              </button>

              {/* Expanded content */}
              {isExpanded && (
                <div className="border-t border-border p-3 space-y-3 bg-muted/30">
                  {/* Arguments */}
                  {Boolean(args) && (
                    <div>
                      <p className="text-xs font-medium text-muted-foreground mb-1">Arguments</p>
                      <pre className="text-xs font-mono bg-background p-2 rounded overflow-auto max-h-32 text-foreground">
                        {JSON.stringify(args, null, 2)}
                      </pre>
                    </div>
                  )}

                  {/* Resolved arguments (if different) */}
                  {Boolean(resolvedArgs) &&
                    JSON.stringify(resolvedArgs) !== JSON.stringify(args) && (
                      <div>
                        <p className="text-xs font-medium text-muted-foreground mb-1">
                          Resolved Arguments
                        </p>
                        <pre className="text-xs font-mono bg-background p-2 rounded overflow-auto max-h-32 text-foreground">
                          {JSON.stringify(resolvedArgs, null, 2)}
                        </pre>
                      </div>
                    )}

                  {/* Response */}
                  {response != null && (
                    <div>
                      <p className="text-xs font-medium text-muted-foreground mb-1">
                        Response ({call.response_type})
                      </p>
                      <pre className="text-xs font-mono bg-background p-2 rounded overflow-auto max-h-48 text-foreground">
                        {typeof response === "string"
                          ? response
                          : JSON.stringify(response, null, 2)}
                      </pre>
                    </div>
                  )}

                  {/* Error message */}
                  {call.error_message && (
                    <div>
                      <p className="text-xs font-medium text-red-400 mb-1">Error</p>
                      <p className="text-xs text-red-300 bg-red-900/20 p-2 rounded">
                        {call.error_message}
                      </p>
                    </div>
                  )}

                  {/* Timestamp */}
                  <div className="flex gap-4 text-xs text-muted-foreground pt-2 border-t border-border">
                    <span>Created: {new Date(call.created_at).toLocaleString()}</span>
                  </div>
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

// MCP Calls Section - only shows SQLite data (MCP calls are only stored to SQLite)
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

// =============================================================================
// Execution Spans Section
// =============================================================================

interface ExecutionSpan {
  id: number;
  execution_id: string | null;
  trace_id: string;
  span_id: string;
  parent_span_id: string | null;
  name: string;
  start_ts: string;
  end_ts: string | null;
  duration_ms: number | null;
  attributes: string | null;
  success: boolean;
  error: string | null;
  created_at: string;
}

function formatDurationMs(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const minutes = Math.floor(ms / 60_000);
  const seconds = Math.floor((ms % 60_000) / 1000);
  return `${minutes}m ${seconds}s`;
}

function getSpanColor(name: string): string {
  if (name.startsWith("workflow.phase.")) return "text-blue-400";
  if (name.startsWith("workflow.")) return "text-blue-300";
  if (name.startsWith("ai.")) return "text-purple-400";
  if (name.startsWith("python.")) return "text-yellow-400";
  if (name.startsWith("playwright.")) return "text-green-400";
  if (name.startsWith("image.")) return "text-orange-400";
  if (name.startsWith("api.")) return "text-cyan-400";
  return "text-muted-foreground";
}

function ExecutionSpansSection() {
  const { selectedRunId } = useRunSelection();
  const [spans, setSpans] = useState<ExecutionSpan[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expandedIds, setExpandedIds] = useState<Set<number>>(new Set());
  const [allSpans, setAllSpans] = useState<ExecutionSpan[]>([]);

  // Fetch spans when selectedRunId changes
  useEffect(() => {
    if (!selectedRunId) {
      setSpans([]);
      setAllSpans([]);
      return;
    }
    const controller = new AbortController();
    let cancelled = false;
    setIsLoading(true);
    setError(null);

    (async () => {
      try {
        const res = await tracedFetch(
          `${getApiBase()}/execution-spans?execution_id=${encodeURIComponent(selectedRunId)}&limit=200`,
          { signal: controller.signal },
        );
        const data = await res.json();
        if (cancelled) return;
        const runSpans = data.spans ?? [];
        setSpans(runSpans);
        setIsLoading(false);

        // If no run-specific spans, fetch recent spans from all executions
        if (runSpans.length === 0) {
          try {
            const allRes = await tracedFetch(`${getApiBase()}/execution-spans?limit=50`, {
              signal: controller.signal,
            });
            const allData = await allRes.json();
            if (!cancelled) setAllSpans(allData.spans ?? []);
          } catch {
            // ignore - may be aborted
          }
        }
      } catch (err) {
        if (!cancelled) {
          setError(String(err));
          setIsLoading(false);
        }
      }
    })();

    return () => {
      cancelled = true;
      controller.abort();
    };
  }, [selectedRunId]);

  const showingAll = spans.length === 0 && allSpans.length > 0;
  const displaySpans = spans.length > 0 ? spans : allSpans;

  const toggleExpanded = (id: number) => {
    setExpandedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Timer className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view execution spans</p>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-destructive">
        <AlertCircle className="w-8 h-8 mb-3" />
        <p className="text-sm">Error loading spans: {error}</p>
      </div>
    );
  }

  // Compute summary stats
  const phaseSpans = displaySpans.filter((s) => s.name.startsWith("workflow.phase."));
  const aiSpans = displaySpans.filter((s) => s.name === "ai.session");
  const totalAiMs = aiSpans.reduce((sum, s) => sum + (s.duration_ms ?? 0), 0);
  const failedSpans = displaySpans.filter((s) => !s.success);
  const slowSpans = displaySpans.filter((s) => (s.duration_ms ?? 0) > 5000);

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h3 className="text-lg font-semibold">Execution Spans</h3>
        <span className="text-xs text-muted-foreground">
          {displaySpans.length} spans{showingAll ? " (all executions)" : ""}
        </span>
      </div>

      {displaySpans.length === 0 ? (
        <div className="text-center py-8 text-muted-foreground">
          <Timer className="w-8 h-8 mx-auto mb-3 opacity-50" />
          <p className="text-sm">No execution spans recorded</p>
          <p className="text-xs mt-1 opacity-70">
            Spans are captured when workflows execute instrumented code paths
          </p>
        </div>
      ) : (
        <>
          {/* Summary Cards */}
          <div className="grid grid-cols-4 gap-3">
            <div className="rounded-lg border border-border bg-card p-3">
              <div className="text-xs text-muted-foreground mb-1">Phases</div>
              <div className="text-lg font-semibold">{phaseSpans.length}</div>
            </div>
            <div className="rounded-lg border border-border bg-card p-3">
              <div className="text-xs text-muted-foreground mb-1">AI Sessions</div>
              <div className="text-lg font-semibold">{aiSpans.length}</div>
              {aiSpans.length > 0 && (
                <div className="text-xs text-muted-foreground">
                  {formatDurationMs(totalAiMs)} total
                </div>
              )}
            </div>
            <div className="rounded-lg border border-border bg-card p-3">
              <div className="text-xs text-muted-foreground mb-1">Slow (&gt;5s)</div>
              <div className="text-lg font-semibold">{slowSpans.length}</div>
            </div>
            <div className="rounded-lg border border-border bg-card p-3">
              <div className="text-xs text-muted-foreground mb-1">Failed</div>
              <div
                className={`text-lg font-semibold ${failedSpans.length > 0 ? "text-destructive" : ""}`}
              >
                {failedSpans.length}
              </div>
            </div>
          </div>

          {/* Phase Timeline */}
          {phaseSpans.length > 0 && (
            <div className="rounded-lg border border-border bg-card p-3">
              <h4 className="text-sm font-medium mb-2">Phase Timings</h4>
              <div className="space-y-1.5">
                {phaseSpans.map((span) => {
                  const phaseName = span.name.replace("workflow.phase.", "");
                  const duration = span.duration_ms ?? 0;
                  const maxDuration = Math.max(...phaseSpans.map((s) => s.duration_ms ?? 0), 1);
                  const widthPct = Math.max((duration / maxDuration) * 100, 2);

                  return (
                    <div key={span.id} className="flex items-center gap-2 text-sm">
                      <span className="w-24 text-muted-foreground truncate">{phaseName}</span>
                      <div className="flex-1 h-5 bg-muted/50 rounded overflow-hidden">
                        <div
                          className={`h-full rounded ${
                            span.success ? "bg-blue-500/60" : "bg-destructive/60"
                          }`}
                          style={{ width: `${widthPct}%` }}
                        />
                      </div>
                      <span className="w-20 text-right text-xs font-mono text-muted-foreground">
                        {formatDurationMs(duration)}
                      </span>
                      {!span.success && (
                        <XCircle className="w-3.5 h-3.5 text-destructive flex-shrink-0" />
                      )}
                    </div>
                  );
                })}
              </div>
            </div>
          )}

          {/* All Spans List */}
          <div className="rounded-lg border border-border bg-card">
            <h4 className="text-sm font-medium p-3 border-b border-border">All Spans</h4>
            <div className="divide-y divide-border max-h-[500px] overflow-y-auto">
              {displaySpans.map((span) => {
                const isExpanded = expandedIds.has(span.id);
                let attrs: Record<string, unknown> = {};
                try {
                  if (span.attributes) attrs = JSON.parse(span.attributes);
                } catch {
                  /* JSON parse may fail for invalid attributes */
                }

                return (
                  <div key={span.id}>
                    <button
                      onClick={() => toggleExpanded(span.id)}
                      className="w-full flex items-center gap-2 px-3 py-2 text-sm hover:bg-muted/50 transition-colors"
                    >
                      {isExpanded ? (
                        <ChevronDown className="w-3.5 h-3.5 flex-shrink-0 text-muted-foreground" />
                      ) : (
                        <ChevronRight className="w-3.5 h-3.5 flex-shrink-0 text-muted-foreground" />
                      )}
                      {span.success ? (
                        <CheckCircle className="w-3.5 h-3.5 flex-shrink-0 text-green-500" />
                      ) : (
                        <XCircle className="w-3.5 h-3.5 flex-shrink-0 text-destructive" />
                      )}
                      <span className={`font-mono text-xs ${getSpanColor(span.name)}`}>
                        {span.name}
                      </span>
                      <span className="flex-1" />
                      {span.duration_ms != null && (
                        <span className="text-xs font-mono text-muted-foreground">
                          {formatDurationMs(span.duration_ms)}
                        </span>
                      )}
                    </button>
                    {isExpanded && (
                      <div className="px-3 pb-3 pl-10 space-y-1.5">
                        <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
                          <div>
                            <span className="text-muted-foreground">Trace ID: </span>
                            <span className="font-mono">{span.trace_id}</span>
                          </div>
                          <div>
                            <span className="text-muted-foreground">Span ID: </span>
                            <span className="font-mono">{span.span_id}</span>
                          </div>
                          {span.parent_span_id && (
                            <div>
                              <span className="text-muted-foreground">Parent: </span>
                              <span className="font-mono">{span.parent_span_id}</span>
                            </div>
                          )}
                          <div>
                            <span className="text-muted-foreground">Start: </span>
                            <span className="font-mono">
                              {new Date(span.start_ts).toLocaleTimeString()}
                            </span>
                          </div>
                          {span.end_ts && (
                            <div>
                              <span className="text-muted-foreground">End: </span>
                              <span className="font-mono">
                                {new Date(span.end_ts).toLocaleTimeString()}
                              </span>
                            </div>
                          )}
                        </div>
                        {span.error && (
                          <div className="text-xs text-destructive bg-destructive/10 rounded p-2 mt-1">
                            {span.error}
                          </div>
                        )}
                        {Object.keys(attrs).length > 0 && (
                          <div className="mt-1">
                            <div className="text-xs text-muted-foreground mb-1">Attributes</div>
                            <pre className="text-xs bg-muted/50 rounded p-2 overflow-x-auto max-h-48">
                              {JSON.stringify(attrs, null, 2)}
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
        </>
      )}
    </div>
  );
}

// =============================================================================
// Mobile Sections
// =============================================================================

import {
  useMobileStates,
  useMobileLogs,
  useMobileErrors,
  useMobileDevices,
  useCaptureFeedback,
} from "../../hooks/useMobileData";
import type { MobileState as _MobileState, MobileLog as _MobileLog } from "../../types/mobile";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

// Mobile State Section - displays device/app state captures
function MobileStateSection() {
  const { selectedRunId } = useRunSelection();
  const { data: states, isLoading, error } = useMobileStates(selectedRunId, 50);
  const { data: devices } = useMobileDevices();
  const captureFeedback = useCaptureFeedback();
  const [expandedIds, setExpandedIds] = useState<Set<number>>(new Set());

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

  const handleCapture = async () => {
    if (!selectedRunId || !devices?.length) return;
    try {
      await captureFeedback.mutateAsync({
        taskRunId: selectedRunId,
        deviceId: devices[0].device_id,
      });
    } catch (err) {
      console.error("Failed to capture mobile feedback:", err);
    }
  };

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Smartphone className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view mobile state</p>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-destructive">
        <AlertCircle className="w-8 h-8 mb-3" />
        <p className="text-sm">Error loading mobile state: {String(error)}</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h3 className="text-lg font-semibold">Mobile App State</h3>
        <div className="flex items-center gap-2">
          {devices && devices.length > 0 && (
            <span className="text-xs text-muted-foreground">
              {devices.length} device(s) connected
            </span>
          )}
          <button
            onClick={handleCapture}
            disabled={captureFeedback.isPending || !devices?.length}
            className="px-3 py-1.5 text-sm rounded-md bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
          >
            {captureFeedback.isPending ? "Capturing..." : "Capture Now"}
          </button>
        </div>
      </div>

      {!states || states.length === 0 ? (
        <div className="text-center py-8 text-muted-foreground">
          <Smartphone className="w-8 h-8 mx-auto mb-3 opacity-50" />
          <p className="text-sm">No mobile state captures for this task run</p>
          <p className="text-xs mt-1">Click "Capture Now" to capture device state</p>
        </div>
      ) : (
        <div className="space-y-2">
          {states.map((state) => (
            <div key={state.id} className="border rounded-lg bg-card overflow-hidden">
              <button
                onClick={() => toggleExpanded(state.id)}
                className="w-full flex items-center gap-3 p-3 hover:bg-muted/50 transition-colors"
              >
                {expandedIds.has(state.id) ? (
                  <ChevronDown className="w-4 h-4 flex-shrink-0" />
                ) : (
                  <ChevronRight className="w-4 h-4 flex-shrink-0" />
                )}
                <div className="flex-1 flex items-center gap-3">
                  <span
                    className={`px-2 py-0.5 text-xs rounded-full ${
                      state.device_type === "emulator"
                        ? getAccentColors("blue").bg + " " + getAccentColors("blue").text
                        : getAccentColors("green").bg + " " + getAccentColors("green").text
                    }`}
                  >
                    {state.device_type || "unknown"}
                  </span>
                  <span className="text-sm font-medium">{state.device_id || "Unknown Device"}</span>
                  {state.has_errors && <AlertCircle className="w-4 h-4 text-destructive" />}
                </div>
                <span className="text-xs text-muted-foreground">
                  {new Date(state.timestamp).toLocaleTimeString()}
                </span>
              </button>

              {expandedIds.has(state.id) && (
                <div className="px-3 pb-3 pt-1 border-t bg-muted/20">
                  <div className="grid grid-cols-2 gap-4 text-sm">
                    <div>
                      <div className="text-muted-foreground text-xs mb-1">App State</div>
                      <div>{state.app_state || "-"}</div>
                    </div>
                    <div>
                      <div className="text-muted-foreground text-xs mb-1">Metro</div>
                      <div className="flex items-center gap-2">
                        <span
                          className={`w-2 h-2 rounded-full ${
                            state.metro_connected ? "bg-green-500" : "bg-muted"
                          }`}
                        />
                        {state.metro_connected ? "Connected" : "Disconnected"}
                      </div>
                    </div>
                    <div>
                      <div className="text-muted-foreground text-xs mb-1">Bundle Status</div>
                      <div>{state.bundle_status || "-"}</div>
                    </div>
                    <div>
                      <div className="text-muted-foreground text-xs mb-1">Last Reload</div>
                      <div>{state.last_reload_type || "-"}</div>
                    </div>
                    {state.screenshot_path && (
                      <div className="col-span-2">
                        <div className="text-muted-foreground text-xs mb-1">Screenshot</div>
                        <div className="text-xs font-mono truncate">{state.screenshot_path}</div>
                      </div>
                    )}
                    {state.error_summary && (
                      <div className="col-span-2">
                        <div className="text-muted-foreground text-xs mb-1">Error Summary</div>
                        <div className="text-destructive text-sm">{state.error_summary}</div>
                      </div>
                    )}
                  </div>
                </div>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// Mobile Screenshots Section - displays captured screenshots
function MobileScreenshotsSection() {
  const { selectedRunId } = useRunSelection();
  const { data: states, isLoading, error } = useMobileStates(selectedRunId, 100);

  // Filter states that have screenshots
  const statesWithScreenshots = useMemo(
    () => states?.filter((s) => s.screenshot_path) || [],
    [states],
  );

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Camera className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view mobile screenshots</p>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-destructive">
        <AlertCircle className="w-8 h-8 mb-3" />
        <p className="text-sm">Error loading screenshots: {String(error)}</p>
      </div>
    );
  }

  if (statesWithScreenshots.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Camera className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">No mobile screenshots for this task run</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <h3 className="text-lg font-semibold">Mobile Screenshots</h3>
      <div className="grid grid-cols-2 gap-4">
        {statesWithScreenshots.map((state) => (
          <div key={state.id} className="border rounded-lg bg-card overflow-hidden">
            <div className="aspect-[9/16] bg-muted relative">
              {state.screenshot_path && (
                <img
                  src={convertFileSrc(state.screenshot_path)}
                  alt={`Screenshot at ${state.timestamp}`}
                  className="w-full h-full object-contain"
                  onError={(e) => {
                    (e.target as HTMLImageElement).style.display = "none";
                  }}
                />
              )}
            </div>
            <div className="p-2 border-t">
              <div className="text-xs text-muted-foreground">
                {new Date(state.timestamp).toLocaleString()}
              </div>
              <div className="text-xs truncate font-mono mt-1">
                {state.device_id || "Unknown Device"}
              </div>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

// Mobile Logs Section - displays parsed log entries
function MobileLogsSection() {
  const { selectedRunId } = useRunSelection();
  const [logSource, setLogSource] = useState<string | undefined>(undefined);
  const { data: logs, isLoading, error } = useMobileLogs(selectedRunId, logSource, false, 500);

  const getLogLevelStyle = (level?: string) => {
    const l = level?.toLowerCase() || "";
    if (l === "e" || l === "error" || l === "fatal" || l === "f") {
      return "text-destructive";
    }
    if (l === "w" || l === "warn" || l === "warning") {
      return "text-yellow-600 dark:text-yellow-500";
    }
    if (l === "d" || l === "debug") {
      return "text-muted-foreground";
    }
    return "text-foreground";
  };

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Terminal className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view mobile logs</p>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-destructive">
        <AlertCircle className="w-8 h-8 mb-3" />
        <p className="text-sm">Error loading logs: {String(error)}</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h3 className="text-lg font-semibold">Mobile Logs</h3>
        <select
          value={logSource || "all"}
          onChange={(e) => setLogSource(e.target.value === "all" ? undefined : e.target.value)}
          className="text-sm border rounded-md px-2 py-1 bg-background"
        >
          <option value="all">All Sources</option>
          <option value="logcat">Logcat</option>
          <option value="metro">Metro</option>
          <option value="build">Build</option>
        </select>
      </div>

      {!logs || logs.length === 0 ? (
        <div className="text-center py-8 text-muted-foreground">
          <Terminal className="w-8 h-8 mx-auto mb-3 opacity-50" />
          <p className="text-sm">No mobile logs for this task run</p>
        </div>
      ) : (
        <div className="space-y-1 font-mono text-xs">
          {logs.map((log) => (
            <div
              key={log.id}
              className={`flex gap-2 p-1 hover:bg-muted/50 rounded ${getLogLevelStyle(log.log_level)}`}
            >
              <span className="text-muted-foreground flex-shrink-0 w-20">
                {new Date(log.timestamp).toLocaleTimeString()}
              </span>
              <span className="flex-shrink-0 w-6 text-center font-bold">
                {log.log_level?.charAt(0).toUpperCase() || "-"}
              </span>
              <span className="flex-shrink-0 w-24 truncate text-muted-foreground">
                {log.log_tag || log.log_source}
              </span>
              <span className="flex-1 break-all">{log.message}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// Mobile Errors Section - displays error logs specifically
function MobileErrorsSection() {
  const { selectedRunId } = useRunSelection();
  const { data: errors, isLoading, error } = useMobileErrors(selectedRunId, 100);
  const [expandedIds, setExpandedIds] = useState<Set<number>>(new Set());

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

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <AlertCircle className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view mobile errors</p>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-destructive">
        <AlertCircle className="w-8 h-8 mb-3" />
        <p className="text-sm">Error loading mobile errors: {String(error)}</p>
      </div>
    );
  }

  if (!errors || errors.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-green-600 dark:text-green-500">
        <CheckCircle className="w-8 h-8 mb-3" />
        <p className="text-sm">No errors detected in mobile logs</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h3 className="text-lg font-semibold text-destructive">Mobile Errors ({errors.length})</h3>
      </div>

      <div className="space-y-2">
        {errors.map((err) => (
          <div
            key={err.id}
            className="border border-destructive/30 rounded-lg bg-destructive/5 overflow-hidden"
          >
            <button
              onClick={() => toggleExpanded(err.id)}
              className="w-full flex items-center gap-3 p-3 hover:bg-destructive/10 transition-colors text-left"
            >
              {expandedIds.has(err.id) ? (
                <ChevronDown className="w-4 h-4 flex-shrink-0" />
              ) : (
                <ChevronRight className="w-4 h-4 flex-shrink-0" />
              )}
              <AlertCircle className="w-4 h-4 flex-shrink-0 text-destructive" />
              <div className="flex-1 min-w-0">
                <div className="text-sm font-medium truncate text-destructive">
                  {err.error_type || err.log_tag || "Error"}
                </div>
                <div className="text-xs text-muted-foreground truncate">{err.message}</div>
              </div>
              <span className="text-xs text-muted-foreground flex-shrink-0">
                {new Date(err.timestamp).toLocaleTimeString()}
              </span>
            </button>

            {expandedIds.has(err.id) && (
              <div className="px-3 pb-3 pt-1 border-t border-destructive/20 space-y-3">
                <div>
                  <div className="text-xs text-muted-foreground mb-1">Message</div>
                  <div className="text-sm">{err.message}</div>
                </div>

                {err.stack_trace && (
                  <div>
                    <div className="text-xs text-muted-foreground mb-1">Stack Trace</div>
                    <pre className="text-xs bg-muted p-2 rounded overflow-x-auto">
                      {err.stack_trace}
                    </pre>
                  </div>
                )}

                {err.file_path && (
                  <div>
                    <div className="text-xs text-muted-foreground mb-1">Location</div>
                    <div className="text-sm font-mono">
                      {err.file_path}
                      {err.line_number && `:${err.line_number}`}
                      {err.column_number && `:${err.column_number}`}
                    </div>
                  </div>
                )}

                {err.raw_line && (
                  <div>
                    <div className="text-xs text-muted-foreground mb-1">Raw Log</div>
                    <pre className="text-xs bg-muted p-2 rounded overflow-x-auto font-mono">
                      {err.raw_line}
                    </pre>
                  </div>
                )}
              </div>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

// Helper to calculate duration between two date strings
function formatSessionDuration(startedAt: string, stoppedAt: string | null): string {
  if (!stoppedAt) return "running";
  try {
    const ms = new Date(stoppedAt).getTime() - new Date(startedAt).getTime();
    if (ms < 0) return "\u2014";
    if (ms < 1000) return `${ms}ms`;
    if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
    const minutes = Math.floor(ms / 60_000);
    const seconds = Math.floor((ms % 60_000) / 1000);
    if (minutes < 60) return `${minutes}m ${seconds}s`;
    const hours = Math.floor(minutes / 60);
    const remainingMinutes = minutes % 60;
    return `${hours}h ${remainingMinutes}m`;
  } catch {
    return "\u2014";
  }
}

// Inline detail panel for an expanded process session
function ProcessSessionDetailPanel({ session }: { session: ProcessSession }) {
  const stateIcon =
    session.state === "running" ? (
      <Activity className="w-3.5 h-3.5 text-green-500" />
    ) : session.state === "failed" ? (
      <XCircle className="w-3.5 h-3.5 text-red-500" />
    ) : session.state === "stopped" ? (
      <CheckCircle className="w-3.5 h-3.5 text-muted-foreground" />
    ) : (
      <Clock className="w-3.5 h-3.5 text-muted-foreground" />
    );

  const stateBadgeClass =
    session.state === "running"
      ? "bg-green-500/10 text-green-500"
      : session.state === "failed"
        ? "bg-red-500/10 text-red-500"
        : "bg-muted text-muted-foreground";

  return (
    <div className="border-t border-border bg-muted/30 px-4 py-3 space-y-3">
      {/* Session metadata header */}
      <div className="grid grid-cols-2 md:grid-cols-4 gap-3 text-xs">
        <div>
          <span className="text-muted-foreground block mb-0.5">Process</span>
          <span className="font-medium flex items-center gap-1.5">
            <Terminal className="w-3 h-3 text-muted-foreground" />
            {session.process_name}
          </span>
        </div>
        <div>
          <span className="text-muted-foreground block mb-0.5">State</span>
          <span
            className={`inline-flex items-center gap-1 rounded-full px-2 py-0.5 font-medium ${stateBadgeClass}`}
          >
            {stateIcon}
            {session.state}
          </span>
        </div>
        <div>
          <span className="text-muted-foreground block mb-0.5">Duration</span>
          <span className="font-mono flex items-center gap-1.5">
            <Timer className="w-3 h-3 text-muted-foreground" />
            {formatSessionDuration(session.started_at, session.stopped_at)}
          </span>
        </div>
        <div>
          <span className="text-muted-foreground block mb-0.5">Exit Code</span>
          <span
            className={`font-mono ${
              session.exit_code !== null && session.exit_code !== 0 ? "text-red-500" : ""
            }`}
          >
            {session.exit_code !== null ? session.exit_code : "\u2014"}
          </span>
        </div>
        <div>
          <span className="text-muted-foreground block mb-0.5">Started</span>
          <span className="font-mono">{formatTimestamp(session.started_at)}</span>
        </div>
        <div>
          <span className="text-muted-foreground block mb-0.5">Stopped</span>
          <span className="font-mono">
            {session.stopped_at ? formatTimestamp(session.stopped_at) : "\u2014"}
          </span>
        </div>
        <div>
          <span className="text-muted-foreground block mb-0.5">Errors</span>
          <span className={session.error_count > 0 ? "text-red-500 font-medium" : ""}>
            {session.error_count > 0 ? (
              <span className="flex items-center gap-1">
                <AlertCircle className="w-3 h-3" />
                {session.error_count}
              </span>
            ) : (
              "0"
            )}
          </span>
        </div>
        <div>
          <span className="text-muted-foreground block mb-0.5">Session ID</span>
          <span
            className="font-mono text-[10px] text-muted-foreground truncate block"
            title={session.id}
          >
            {session.id}
          </span>
        </div>
      </div>

      {/* Inline output viewer */}
      <div className="border-t border-border pt-2">
        <ProcessSessionOutputSection sessionId={session.id} compact />
      </div>
    </div>
  );
}

// Process Sessions Section
function ProcessSessionsSection({
  onSelectSession,
}: {
  onSelectSession?: (sessionId: string) => void;
}) {
  const { data: sessions, isLoading, error } = useProcessSessions();
  const [expandedSessionId, setExpandedSessionId] = useState<string | null>(null);

  if (isLoading) {
    return (
      <div className="p-4 text-muted-foreground flex items-center gap-2">
        <Loader2 className="w-4 h-4 animate-spin" />
        Loading sessions...
      </div>
    );
  }
  if (error) {
    return <div className="p-4 text-red-500">Error: {String(error)}</div>;
  }
  if (!sessions || sessions.length === 0) {
    return <div className="p-4 text-muted-foreground">No process sessions found.</div>;
  }

  const handleRowClick = (session: ProcessSession) => {
    setExpandedSessionId((prev) => (prev === session.id ? null : session.id));
    onSelectSession?.(session.id);
  };

  return (
    <div className="space-y-1">
      <h3 className="px-4 pt-4 pb-2 text-sm font-medium text-muted-foreground">
        Process Sessions ({sessions.length})
      </h3>
      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="border-b text-left text-muted-foreground">
              <th className="px-2 py-2 w-8"></th>
              <th className="px-4 py-2">Process</th>
              <th className="px-4 py-2">Started</th>
              <th className="px-4 py-2">Duration</th>
              <th className="px-4 py-2">State</th>
              <th className="px-4 py-2">Exit Code</th>
              <th className="px-4 py-2">Errors</th>
            </tr>
          </thead>
          <tbody>
            {sessions.map((session) => {
              const isExpanded = expandedSessionId === session.id;
              return (
                <Fragment key={session.id}>
                  <tr
                    className={`border-b hover:bg-muted/50 cursor-pointer ${
                      isExpanded ? "bg-muted/30" : ""
                    }`}
                    onClick={() => handleRowClick(session)}
                  >
                    <td className="px-2 py-2">
                      {isExpanded ? (
                        <ChevronDown className="w-4 h-4 text-muted-foreground" />
                      ) : (
                        <ChevronRight className="w-4 h-4 text-muted-foreground" />
                      )}
                    </td>
                    <td className="px-4 py-2 font-medium">{session.process_name}</td>
                    <td className="px-4 py-2 text-muted-foreground">
                      {formatTimestamp(session.started_at)}
                    </td>
                    <td className="px-4 py-2 text-muted-foreground font-mono text-xs">
                      {formatSessionDuration(session.started_at, session.stopped_at)}
                    </td>
                    <td className="px-4 py-2">
                      <span
                        className={`inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium ${
                          session.state === "running"
                            ? "bg-green-500/10 text-green-500"
                            : session.state === "failed"
                              ? "bg-red-500/10 text-red-500"
                              : "bg-muted text-muted-foreground"
                        }`}
                      >
                        {session.state}
                      </span>
                    </td>
                    <td className="px-4 py-2 font-mono">
                      {session.exit_code !== null ? session.exit_code : "\u2014"}
                    </td>
                    <td className="px-4 py-2">
                      {session.error_count > 0 ? (
                        <span className="text-red-500">{session.error_count}</span>
                      ) : (
                        "0"
                      )}
                    </td>
                  </tr>
                  {isExpanded && (
                    <tr>
                      <td colSpan={7} className="p-0">
                        <ProcessSessionDetailPanel session={session} />
                      </td>
                    </tr>
                  )}
                </Fragment>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}

// Process Session Output Section
function ProcessSessionOutputSection({
  sessionId,
  compact = false,
}: {
  sessionId: string | null;
  compact?: boolean;
}) {
  const { data: lines, isLoading, error } = useProcessSessionOutput(sessionId);
  const [streamFilter, setStreamFilter] = useState<"all" | "stdout" | "stderr">("all");
  const [searchText, setSearchText] = useState("");

  const filteredLines = useMemo(() => {
    if (!lines) return [];
    return lines.filter((line) => {
      if (streamFilter !== "all" && line.stream !== streamFilter) return false;
      if (searchText && !line.line.toLowerCase().includes(searchText.toLowerCase())) return false;
      return true;
    });
  }, [lines, streamFilter, searchText]);

  const stdoutCount = useMemo(
    () => lines?.filter((l) => l.stream === "stdout").length ?? 0,
    [lines],
  );
  const stderrCount = useMemo(
    () => lines?.filter((l) => l.stream === "stderr").length ?? 0,
    [lines],
  );

  if (!sessionId) {
    return (
      <div className={`${compact ? "p-2" : "p-4"} text-muted-foreground`}>
        Select a session from the Sessions tab to view output.
      </div>
    );
  }
  if (isLoading) {
    return (
      <div className={`${compact ? "p-2" : "p-4"} text-muted-foreground flex items-center gap-2`}>
        <Loader2 className="w-3 h-3 animate-spin" />
        Loading output...
      </div>
    );
  }
  if (error) {
    return <div className={`${compact ? "p-2" : "p-4"} text-red-500`}>Error: {String(error)}</div>;
  }
  if (!lines || lines.length === 0) {
    return (
      <div className={`${compact ? "p-2" : "p-4"} text-muted-foreground`}>
        No output captured for this session.
      </div>
    );
  }

  const isFiltered = streamFilter !== "all" || searchText.length > 0;

  return (
    <div className={compact ? "flex flex-col" : "flex flex-col h-full"}>
      <div className={compact ? "pb-1 space-y-2" : "px-4 pt-4 pb-2 space-y-3"}>
        <h3 className="text-sm font-medium text-muted-foreground">
          Session Output
          {isFiltered
            ? ` \u2014 Showing ${filteredLines.length} of ${lines.length} lines`
            : ` (${lines.length} lines)`}
        </h3>

        {/* Toolbar: stream filter + text search */}
        <div className="flex flex-wrap items-center gap-3">
          {/* Stream filter buttons */}
          <div className="flex gap-1.5">
            <button
              onClick={() => setStreamFilter("all")}
              className={`px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
                streamFilter === "all"
                  ? `${getAccentColors("blue").bg} ${getAccentColors("blue").text} border ${getAccentColors("blue").border}`
                  : "bg-muted text-muted-foreground hover:text-foreground"
              }`}
            >
              All ({lines.length})
            </button>
            <button
              onClick={() => setStreamFilter("stdout")}
              className={`px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
                streamFilter === "stdout"
                  ? `${getAccentColors("green").bg} ${getAccentColors("green").text} border ${getAccentColors("green").border}`
                  : "bg-muted text-muted-foreground hover:text-foreground"
              }`}
            >
              stdout ({stdoutCount})
            </button>
            <button
              onClick={() => setStreamFilter("stderr")}
              className={`px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
                streamFilter === "stderr"
                  ? `${getAccentColors("red").bg} ${getAccentColors("red").text} border ${getAccentColors("red").border}`
                  : "bg-muted text-muted-foreground hover:text-foreground"
              }`}
            >
              stderr ({stderrCount})
            </button>
          </div>

          {/* Text search input */}
          <div className="relative flex-1 min-w-[200px]">
            <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-muted-foreground pointer-events-none" />
            <input
              type="text"
              placeholder="Filter lines by text..."
              value={searchText}
              onChange={(e) => setSearchText(e.target.value)}
              className="w-full pl-8 pr-3 py-1.5 text-xs rounded-md border border-border bg-muted/50 text-foreground placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-ring"
            />
            {searchText && (
              <button
                onClick={() => setSearchText("")}
                className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground text-xs"
                title="Clear search"
              >
                &times;
              </button>
            )}
          </div>
        </div>
      </div>

      <div className={compact ? "overflow-auto" : "flex-1 overflow-auto px-4 pb-4"}>
        {filteredLines.length === 0 ? (
          <div className="text-sm text-muted-foreground py-4">
            No lines match the current filters.
          </div>
        ) : (
          <pre
            className={`text-xs font-mono bg-black/80 text-green-400 rounded-lg overflow-auto ${
              compact ? "p-2 max-h-[40vh]" : "p-4 max-h-[70vh]"
            }`}
          >
            {filteredLines.map((line) => (
              <div key={line.id} className={line.stream === "stderr" ? "text-red-400" : ""}>
                <span className="text-muted-foreground select-none">
                  {new Date(line.timestamp).toLocaleTimeString()}{" "}
                </span>
                {line.line}
              </div>
            ))}
          </pre>
        )}
      </div>
    </div>
  );
}

export function AiDataViewerTab() {
  const [activeCategory, setActiveCategory] = useState<DataCategory>("ai-prompt");
  const [selectedProcessSessionId, _setSelectedProcessSessionId] = useState<string | null>(null);

  // Render the content for the active category
  const renderContent = () => {
    switch (activeCategory) {
      // AI Session group
      case "ai-prompt":
        return <AiPromptSection />;
      case "ai-output":
        return <AiOutputSection />;
      case "contexts":
        return <ContextsSection />;
      // Execution Logs group
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
      // Captures group
      case "dom-snapshots":
        return <DomSnapshotsPanel />;
      // Mobile group
      case "mobile-state":
        return <MobileStateSection />;
      case "mobile-screenshots":
        return <MobileScreenshotsSection />;
      case "mobile-logs":
        return <MobileLogsSection />;
      case "mobile-errors":
        return <MobileErrorsSection />;
      // Processes group
      case "process-sessions":
        return <ProcessSessionsSection />;
      case "process-output":
        return <ProcessSessionOutputSection sessionId={selectedProcessSessionId} />;
      // Configuration group
      case "loaded-config":
        return <LoadedConfigSection />;
      case "dev-logs":
        return <DevLogsSection />;
      default:
        return null;
    }
  };

  // Check if the current category needs flex layout (for scrollable content)
  const needsFlexLayout = ["ai-prompt", "loaded-config"].includes(activeCategory);

  return (
    <div className="h-full flex overflow-hidden">
      {/* Sidebar navigation */}
      <SidebarNav activeCategory={activeCategory} onCategoryChange={setActiveCategory} />

      {/* Content area */}
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
