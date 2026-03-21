import { useCallback, useMemo, useState, useRef, useEffect, type RefObject } from "react";
import { instanceStorage } from "@/lib/instance-storage";
import {
  Maximize2,
  Terminal,
  ChevronDown,
  Check,
  X,
  Send,
  Pin,
  AlertTriangle,
  Copy,
  Filter,
  RotateCcw,
  ArrowDownToLine,
  Download,
  RefreshCw,
} from "lucide-react";
import {
  TerminalInstance,
  type TerminalInstanceHandle,
  type ShellIntegrationEvent,
} from "./TerminalInstance";
import { PlanViewer } from "./PlanViewer";
import type { TerminalTab } from "./useTerminalManager";
import type { LayoutPreset, ZoneAssignments, SessionState } from "./useZoneLayout";

export type ViewMode = "auto" | "full" | "compact";

interface ZoneGridProps {
  layout: LayoutPreset;
  assignments: ZoneAssignments;
  tabs: TerminalTab[];
  focusedZone: number;
  maximizedZone: number | null;
  sessionStates: Record<string, SessionState>;
  /** Last few output lines per tab for compact view */
  lastOutputLines: Record<string, string[]>;
  /** "auto" = focused gets full, others compact when 4+ zones */
  viewMode: ViewMode;
  terminalRefs: Map<string, RefObject<TerminalInstanceHandle | null>>;
  onZoneClick: (zoneIndex: number, ctrlKey?: boolean) => void;
  onZoneDoubleClick: (zoneIndex: number) => void;
  onExit: (terminalId: string, exitCode: number | null) => void;
  onFirstInput: (terminalId: string, input: string) => void;
  onShellIntegration: (tabId: string, event: ShellIntegrationEvent) => void;
  onOutput: (tabId: string, text: string) => void;
  onReconnected: (tabId: string) => void;
  onAssignTab?: (zoneIndex: number, tabId: string) => void;
  /** Tab IDs that recently transitioned to needs-input (for flash animation) */
  flashingTabs?: Set<string>;
  /** Formatted duration strings per tab (e.g., "2m", "45s") */
  stateDurations?: Record<string, string>;
  /** Zones selected for batch operations (Ctrl+click) */
  selectedZones?: Set<number>;
  /** Tab IDs for "working" sessions with no output for 60s+ */
  staleTabs?: Set<string>;
  /** Zones pinned to always show full terminal in auto mode */
  pinnedZones?: Set<number>;
  /** Toggle pin on a zone */
  onTogglePin?: (zoneIndex: number) => void;
  /** Search query to highlight matching output in compact cards */
  outputSearchQuery?: string;
  /** Zone index marked as swap source (Ctrl+Shift+X first press) */
  swapSource?: number | null;
  /** Activity sparkline data per tab (ring buffer of output byte counts) */
  activityData?: Record<string, number[]>;
  /** Editable group labels per zone */
  zoneLabels?: Record<number, string>;
  /** Callback to set a zone label */
  onSetZoneLabel?: (zoneIndex: number, label: string) => void;
  /** Callback to restart a completed/errored terminal in its zone */
  onRestartInZone?: (zoneIndex: number) => void;
  /** Incrementing key to trigger reset of column/row ratios to equal */
  resetRatiosKey?: number;
  /** Label → color mapping for group color coding */
  labelColorMap?: Record<string, string>;
  /** Parsed tags per zone (split from comma-separated zoneLabels) */
  zoneTags?: Record<number, string[]>;
  /** Command history per tab (from shell integration) */
  commandHistories?: Record<string, { command: string; exitCode: number; timestamp: number }[]>;
  /** Whether focus mode is active (dim non-focused zones) */
  focusMode?: boolean;
  /** Freeform notes per zone */
  zoneNotes?: Record<number, string>;
  /** Callback to set a zone note */
  onSetZoneNote?: (zoneIndex: number, note: string) => void;
  /** Export a single zone's output in the given format */
  onExportZone?: (zoneIndex: number, format: "text" | "markdown" | "json") => void;
  /** Zones with pending auto-restarts: zoneIndex -> timestamp when restart fires */
  pendingRestarts?: Record<number, number>;
  /** Cancel a pending auto-restart for a zone */
  onCancelRestart?: (zoneIndex: number) => void;
}

const STATE_BORDER_COLORS: Record<SessionState, string> = {
  idle: "#2a2d3d",
  working: "#7aa2f7",
  "needs-input": "#e0af68",
  completed: "#9ece6a",
  error: "#f7768e",
};

const STATE_COLORS: Record<SessionState, string> = {
  idle: "#565f89",
  working: "#7aa2f7",
  "needs-input": "#e0af68",
  completed: "#9ece6a",
  error: "#f7768e",
};

const STATE_GLOW: Record<SessionState, string> = {
  idle: "none",
  working: "0 0 4px rgba(122, 162, 247, 0.3)",
  "needs-input": "0 0 8px rgba(224, 175, 104, 0.4)",
  completed: "none",
  error: "0 0 4px rgba(247, 118, 142, 0.3)",
};

const STATE_LABELS: Record<SessionState, string> = {
  idle: "Idle",
  working: "Working",
  "needs-input": "Needs Input",
  completed: "Completed",
  error: "Error",
};

const STATE_BG_COLORS: Record<SessionState, string> = {
  idle: "bg-[#565f89]/10",
  working: "bg-[#7aa2f7]/10",
  "needs-input": "bg-[#e0af68]/15",
  completed: "bg-[#9ece6a]/10",
  error: "bg-[#f7768e]/10",
};

function ZoneLabel({
  tab,
  state,
  zoneIndex,
  allTabs,
  assignments,
  sessionStates,
  onAssignTab,
  isPinned,
  onTogglePin,
  zoneLabel,
  onSetZoneLabel,
  onScrollToBottom,
  outputLineCount,
  outputByteSize,
  onToggleFilter,
  filterActive,
}: {
  tab: TerminalTab;
  state: SessionState;
  zoneIndex: number;
  allTabs: TerminalTab[];
  assignments: ZoneAssignments;
  sessionStates: Record<string, SessionState>;
  onAssignTab?: (zoneIndex: number, tabId: string) => void;
  isPinned?: boolean;
  onTogglePin?: () => void;
  zoneLabel?: string;
  onSetZoneLabel?: (label: string) => void;
  onScrollToBottom?: () => void;
  outputLineCount?: number;
  outputByteSize?: number;
  onToggleFilter?: () => void;
  filterActive?: boolean;
}) {
  const [showSelector, setShowSelector] = useState(false);
  const [editingLabel, setEditingLabel] = useState(false);
  const [labelValue, setLabelValue] = useState(zoneLabel ?? "");
  const selectorRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!showSelector) return;
    const handleClick = (e: MouseEvent) => {
      if (selectorRef.current && !selectorRef.current.contains(e.target as Node)) {
        setShowSelector(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [showSelector]);

  return (
    <div
      className="absolute top-0 left-0 right-0 flex items-center gap-1.5 px-2 py-0.5 bg-[#13141f]/80 backdrop-blur-sm z-10 cursor-grab active:cursor-grabbing"
      draggable
      onDragStart={(e) => {
        e.dataTransfer.setData("text/tab-id", tab.id);
        e.dataTransfer.effectAllowed = "move";
      }}
    >
      <div
        className={`w-1.5 h-1.5 rounded-full shrink-0 ${
          state === "needs-input" ? "animate-pulse" : ""
        }`}
        style={{ backgroundColor: STATE_BORDER_COLORS[state] }}
      />
      <span className="text-[10px] text-[#a9b1d6] truncate font-medium">{tab.title}</span>

      {/* Output line count and byte size */}
      {outputLineCount !== undefined && outputLineCount > 0 && (
        <span className="text-[9px] text-[#565f89] font-mono ml-1 shrink-0">
          {outputLineCount > 999 ? `${(outputLineCount / 1000).toFixed(1)}k` : outputLineCount}{" "}
          lines
          {outputByteSize !== undefined && outputByteSize > 1024 && (
            <span className="ml-1">
              (
              {outputByteSize > 1048576
                ? `${(outputByteSize / 1048576).toFixed(1)}MB`
                : `${(outputByteSize / 1024).toFixed(0)}KB`}
              )
            </span>
          )}
        </span>
      )}

      {/* Zone group label */}
      {onSetZoneLabel &&
        (editingLabel ? (
          <input
            autoFocus
            value={labelValue}
            onChange={(e) => setLabelValue(e.target.value)}
            onBlur={() => {
              setEditingLabel(false);
              onSetZoneLabel(labelValue.trim());
            }}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === "Enter") {
                setEditingLabel(false);
                onSetZoneLabel(labelValue.trim());
              }
              if (e.key === "Escape") {
                setEditingLabel(false);
                setLabelValue(zoneLabel ?? "");
              }
            }}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => e.stopPropagation()}
            className="bg-transparent border-b border-[#565f89] text-[9px] text-[#bb9af7] outline-hidden w-16 shrink-0"
            placeholder="Label..."
          />
        ) : zoneLabel ? (
          <span
            className="text-[9px] text-[#bb9af7] bg-[#bb9af7]/10 px-1 py-0 rounded cursor-pointer hover:bg-[#bb9af7]/20 transition-colors shrink-0"
            onDoubleClick={(e) => {
              e.stopPropagation();
              setLabelValue(zoneLabel);
              setEditingLabel(true);
            }}
            title="Double-click to edit"
          >
            {zoneLabel}
          </span>
        ) : (
          <span
            className="text-[9px] text-[#565f89]/30 cursor-pointer hover:text-[#565f89] transition-colors shrink-0"
            onDoubleClick={(e) => {
              e.stopPropagation();
              setLabelValue("");
              setEditingLabel(true);
            }}
            title="Double-click to add label"
          >
            +
          </span>
        ))}

      {/* Tab selector dropdown */}
      {onAssignTab && allTabs.length > 1 && (
        <div className="relative shrink-0" ref={selectorRef}>
          <button
            onClick={(e) => {
              e.stopPropagation();
              setShowSelector(!showSelector);
            }}
            className="p-0.5 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
            title="Switch session in this zone"
          >
            <ChevronDown className="w-2.5 h-2.5" />
          </button>

          {showSelector && (
            <div className="absolute left-0 top-full mt-0.5 w-48 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden">
              <div className="max-h-48 overflow-y-auto scrollbar-dark">
                {allTabs.map((t) => {
                  const isCurrent = t.id === tab.id;
                  const assignedZone = Object.entries(assignments).find(([, id]) => id === t.id);
                  const tabState = sessionStates[t.id] ?? "idle";

                  return (
                    <button
                      key={t.id}
                      onClick={(e) => {
                        e.stopPropagation();
                        onAssignTab(zoneIndex, t.id);
                        setShowSelector(false);
                      }}
                      className={`w-full flex items-center gap-1.5 px-2 py-1 text-left transition-colors ${
                        isCurrent
                          ? "bg-[#7aa2f7]/10 text-[#7aa2f7]"
                          : "text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
                      }`}
                    >
                      <div
                        className={`w-1.5 h-1.5 rounded-full shrink-0 ${tabState === "needs-input" ? "animate-pulse" : ""}`}
                        style={{ backgroundColor: STATE_BORDER_COLORS[tabState] }}
                      />
                      <span className="text-[10px] truncate flex-1">{t.title}</span>
                      {assignedZone && !isCurrent && (
                        <span className="text-[8px] text-[#565f89]">
                          Z{Number(assignedZone[0]) + 1}
                        </span>
                      )}
                      {!assignedZone && (
                        <span className="text-[8px] text-[#565f89] italic">hidden</span>
                      )}
                    </button>
                  );
                })}
              </div>
            </div>
          )}
        </div>
      )}

      {/* Pin toggle */}
      {onTogglePin && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onTogglePin();
          }}
          className={`p-0.5 rounded transition-colors shrink-0 ${
            isPinned
              ? "text-[#7aa2f7] bg-[#7aa2f7]/15"
              : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]"
          }`}
          title={
            isPinned
              ? "Unpin zone (show compact in auto mode)"
              : "Pin zone (keep full terminal in auto mode)"
          }
        >
          <Pin className="w-2.5 h-2.5" />
        </button>
      )}

      {/* Filter output */}
      {onToggleFilter && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onToggleFilter();
          }}
          onMouseDown={(e) => e.stopPropagation()}
          className={`p-0.5 rounded transition-colors shrink-0 ${
            filterActive
              ? "text-[#e0af68] bg-[#e0af68]/10"
              : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
          }`}
          title="Filter output"
        >
          <Filter className="w-2.5 h-2.5" />
        </button>
      )}

      {/* Scroll to bottom */}
      {onScrollToBottom && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onScrollToBottom();
          }}
          onMouseDown={(e) => e.stopPropagation()}
          className="p-0.5 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50 transition-colors shrink-0"
          title="Scroll to bottom"
        >
          <ArrowDownToLine className="w-2.5 h-2.5" />
        </button>
      )}

      {tab.workingDir && (
        <span className="text-[9px] text-[#565f89] truncate ml-auto pointer-events-none">
          {tab.workingDir.split(/[/\\]/).pop()}
        </span>
      )}
    </div>
  );
}

/** Format uptime from a creation timestamp */
function formatUptime(createdAt?: number): string | undefined {
  if (!createdAt) return undefined;
  const ms = Date.now() - createdAt;
  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const remainMin = minutes % 60;
  return `${hours}h${remainMin > 0 ? `${remainMin}m` : ""}`;
}

/** Check if a line looks like a prompt waiting for input */
function isActionableLine(line: string): boolean {
  return /\? \(y\/n\)|\? \(yes\/no\)|Allow .+\? \(|Do you want to proceed|\? >|\(Y\/n\)|\[y\/N\]/i.test(
    line,
  );
}

/** Check if the prompt is a yes/no type (show approve/reject) vs free text (show input) */
function isYesNoPrompt(lines: string[]): boolean {
  return lines.some((line) =>
    /\? \(y\/n\)|\? \(yes\/no\)|\(Y\/n\)|\[y\/N\]|Allow .+\? \(/i.test(line),
  );
}

/** Tiny inline sparkline SVG showing output activity */
/** Compute output trend from activity data: "up" | "down" | "stable" | null */
function computeOutputTrend(
  data: number[] | undefined,
): { trend: "up" | "down" | "stable"; rate: number; peak: number } | null {
  if (!data || data.length < 4) return null;
  const recent = data.slice(-5);
  const earlier = data.slice(-10, -5);
  if (earlier.length < 2) return null;
  const recentAvg = recent.reduce((a, b) => a + b, 0) / recent.length;
  const earlierAvg = earlier.reduce((a, b) => a + b, 0) / earlier.length;
  const peak = Math.max(...data, 1);
  const rate = recentAvg;
  if (earlierAvg === 0 && recentAvg === 0) return { trend: "stable", rate, peak };
  const ratio = earlierAvg > 0 ? recentAvg / earlierAvg : recentAvg > 0 ? 2 : 1;
  if (ratio > 1.3) return { trend: "up", rate, peak };
  if (ratio < 0.7) return { trend: "down", rate, peak };
  return { trend: "stable", rate, peak };
}

const TREND_ICONS: Record<string, { symbol: string; color: string }> = {
  up: { symbol: "▲", color: "#9ece6a" },
  down: { symbol: "▼", color: "#f7768e" },
  stable: { symbol: "―", color: "#565f89" },
};

function ActivitySparkline({ data }: { data: number[] }) {
  if (data.length < 2) return null;
  const max = Math.max(...data, 1);
  const w = 48;
  const h = 12;
  const points = data
    .map((v, i) => {
      const x = (i / (data.length - 1)) * w;
      const y = h - (v / max) * (h - 1);
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");
  return (
    <svg width={w} height={h} className="shrink-0 opacity-60">
      <polyline
        points={points}
        fill="none"
        stroke="#7aa2f7"
        strokeWidth="1"
        strokeLinejoin="round"
      />
    </svg>
  );
}

/** Highlight search query matches within a string */
function HighlightedText({ text, query }: { text: string; query?: string }) {
  if (!query || !text) return <>{text}</>;
  const lower = text.toLowerCase();
  const qLower = query.toLowerCase();
  const idx = lower.indexOf(qLower);
  if (idx === -1) return <>{text}</>;
  return (
    <>
      {text.slice(0, idx)}
      <span className="bg-[#e0af68]/30 text-[#e0af68] rounded-xs px-0.5">
        {text.slice(idx, idx + query.length)}
      </span>
      {text.slice(idx + query.length)}
    </>
  );
}

/** Check if a line looks like an error/failure message */
function _isErrorLine(line: string): boolean {
  const lower = line.toLowerCase();
  return /\b(error|failed|failure|exception|panic|fatal|critical|traceback|segfault)\b/.test(lower);
}

/** Count lines matching a filter string (case-insensitive, minimum 2 chars) */
function countMatches(lines: string[], filter: string): number {
  if (!filter || filter.length < 2) return 0;
  const lower = filter.toLowerCase();
  return lines.filter((l) => l.toLowerCase().includes(lower)).length;
}

/** Compact view card shown in non-focused zones when there are many zones */
function CompactZoneCard({
  tab,
  state,
  zoneIndex,
  lastLines,
  onQuickApprove,
  onQuickReject,
  onSendCommand,
  duration,
  isStale,
  searchQuery,
  activity,
  zoneLabel,
  onSetZoneLabel,
  onRestart,
  groupColor,
  uptime,
  lastCommand,
  note,
  onSetNote,
  allTabs,
  assignments,
  sessionStates,
  onAssignTab,
  tagColor,
}: {
  tab: TerminalTab;
  state: SessionState;
  zoneIndex: number;
  lastLines: string[];
  onQuickApprove?: () => void;
  onQuickReject?: () => void;
  onSendCommand?: (text: string) => void;
  duration?: string;
  isStale?: boolean;
  searchQuery?: string;
  activity?: number[];
  zoneLabel?: string;
  onSetZoneLabel?: (label: string) => void;
  onRestart?: () => void;
  groupColor?: string;
  uptime?: string;
  lastCommand?: string;
  note?: string;
  onSetNote?: (note: string) => void;
  allTabs?: TerminalTab[];
  assignments?: ZoneAssignments;
  sessionStates?: Record<string, SessionState>;
  onAssignTab?: (zoneIndex: number, tabId: string) => void;
  tagColor?: string;
}) {
  const needsInput = state === "needs-input";
  const [commandText, setCommandText] = useState("");
  const [isHovered, setIsHovered] = useState(false);
  const [editingLabel, setEditingLabel] = useState(false);
  const [labelValue, setLabelValue] = useState(zoneLabel ?? "");
  const [editingNote, setEditingNote] = useState(false);
  const [noteValue, setNoteValue] = useState(note ?? "");
  const [copied, setCopied] = useState(false);
  const [outputFilter, setOutputFilter] = useState("");
  const [showFilter, setShowFilter] = useState(false);
  const [showSwitch, setShowSwitch] = useState(false);
  const switchRef = useRef<HTMLDivElement>(null);
  const hoverTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const yesNo = needsInput && isYesNoPrompt(lastLines);
  const outputRef = useRef<HTMLDivElement>(null);

  // Compute spin speed for the working-state progress ring based on recent activity
  const spinSpeed = useMemo(() => {
    if (state !== "working" || !activity || activity.length < 2) return "2s";
    const recent = activity.slice(-3);
    const avg = recent.reduce((a, b) => a + b, 0) / recent.length;
    if (avg > 100) return "0.5s";
    if (avg > 50) return "1s";
    if (avg > 10) return "1.5s";
    return "2.5s"; // slow when little activity
  }, [state, activity]);

  // Auto-scroll to bottom when new output arrives
  useEffect(() => {
    if (outputRef.current) {
      outputRef.current.scrollTop = outputRef.current.scrollHeight;
    }
  }, [lastLines]);

  // Close quick-switch dropdown on outside click or Escape
  useEffect(() => {
    if (!showSwitch) return;
    const handleMouseDown = (e: MouseEvent) => {
      if (switchRef.current && !switchRef.current.contains(e.target as Node)) {
        setShowSwitch(false);
      }
    };
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") setShowSwitch(false);
    };
    document.addEventListener("mousedown", handleMouseDown);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("mousedown", handleMouseDown);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [showSwitch]);

  const handleMouseEnter = useCallback(() => {
    hoverTimerRef.current = setTimeout(() => setIsHovered(true), 300);
  }, []);

  const handleMouseLeave = useCallback(() => {
    if (hoverTimerRef.current) clearTimeout(hoverTimerRef.current);
    setIsHovered(false);
  }, []);

  const searchMatchCount =
    searchQuery && searchQuery.length >= 2
      ? lastLines.filter((l) => l.toLowerCase().includes(searchQuery.toLowerCase())).length
      : 0;

  return (
    <div
      className="h-full w-full flex flex-col p-2 gap-1.5 select-none cursor-pointer relative group"
      onMouseEnter={handleMouseEnter}
      onMouseLeave={handleMouseLeave}
    >
      {/* Thin colored strip at top edge based on tag color */}
      <div
        className="h-[3px] w-full rounded-t shrink-0"
        style={{ backgroundColor: tagColor ? `${tagColor}40` : "#2a2d3d20" }}
      />
      {/* Copy output button — appears on hover, top-right corner */}
      {lastLines.length > 0 && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            if (lastLines.length === 0) return;
            navigator.clipboard.writeText(lastLines.join("\n")).then(() => {
              setCopied(true);
              setTimeout(() => setCopied(false), 1500);
            });
          }}
          className={`absolute top-1 right-1 p-0.5 rounded transition-all z-10 ${
            copied
              ? "text-[#9ece6a] opacity-100"
              : "text-[#565f89] opacity-0 group-hover:opacity-100 hover:text-[#a9b1d6] hover:bg-[#2a2d3d]"
          }`}
          title={copied ? "Copied!" : "Copy output to clipboard"}
        >
          {copied ? <Check className="w-3 h-3" /> : <Copy className="w-3 h-3" />}
        </button>
      )}
      {/* Search match count badge */}
      {searchQuery && searchMatchCount > 0 && (
        <span className="absolute top-1 right-6 text-[8px] font-mono bg-[#e0af68]/20 text-[#e0af68] rounded px-1 z-10">
          {searchMatchCount}
        </span>
      )}
      {/* Header row: zone number, name, state badge */}
      <div className="flex items-center gap-1.5 shrink-0">
        <div className="relative shrink-0" ref={switchRef}>
          <span
            className="text-[9px] text-[#565f89] font-mono w-3 shrink-0 cursor-grab active:cursor-grabbing hover:text-[#a9b1d6] transition-colors"
            role="button"
            tabIndex={0}
            draggable
            onDragStart={(e) => {
              e.dataTransfer.setData("text/zone-index", String(zoneIndex));
              e.dataTransfer.effectAllowed = "move";
              e.stopPropagation();
            }}
            onClick={(e) => {
              e.stopPropagation();
              setShowSwitch((prev) => !prev);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.stopPropagation();
                setShowSwitch((prev) => !prev);
              }
            }}
            title="Click to quick-switch session, drag to swap zones"
          >
            {zoneIndex + 1}
          </span>
          {showSwitch && allTabs && assignments && sessionStates && onAssignTab && (
            <div
              role="presentation"
              className="absolute top-full left-0 mt-1 min-w-[140px] max-h-[150px] overflow-y-auto bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50"
              onMouseDown={(e) => e.stopPropagation()}
              onClick={(e) => e.stopPropagation()}
            >
              {(() => {
                const currentTabId = assignments[zoneIndex];
                const otherTabs = allTabs.filter((t) => t.id !== currentTabId);
                if (otherTabs.length === 0) {
                  return (
                    <div className="px-2 py-1.5 text-[10px] text-[#565f89]">No other sessions</div>
                  );
                }
                return otherTabs.map((t) => {
                  const tState = sessionStates[t.id] ?? "idle";
                  return (
                    <button
                      key={t.id}
                      className="flex items-center gap-1.5 w-full px-2 py-1 text-left hover:bg-[#2a2d3d] transition-colors"
                      onClick={(e) => {
                        e.stopPropagation();
                        onAssignTab(zoneIndex, t.id);
                        setShowSwitch(false);
                      }}
                    >
                      <div
                        className="w-1.5 h-1.5 rounded-full shrink-0"
                        style={{ backgroundColor: STATE_BORDER_COLORS[tState] }}
                      />
                      <span className="text-[10px] text-[#c0caf5] truncate">{t.title}</span>
                    </button>
                  );
                });
              })()}
            </div>
          )}
        </div>
        {state === "working" ? (
          <svg width={12} height={12} className="shrink-0" viewBox="0 0 12 12">
            <circle
              cx={6}
              cy={6}
              r={4.5}
              fill="none"
              stroke="#7aa2f7"
              strokeWidth={1.5}
              opacity={0.2}
            />
            <circle
              cx={6}
              cy={6}
              r={4.5}
              fill="none"
              stroke="#7aa2f7"
              strokeWidth={1.5}
              strokeDasharray="14 14"
              strokeLinecap="round"
              className="animate-spin"
              style={{ transformOrigin: "center", animationDuration: spinSpeed }}
            />
          </svg>
        ) : (
          <div
            className={`w-2 h-2 rounded-full shrink-0 ${state === "needs-input" ? "animate-pulse" : ""}`}
            style={{ backgroundColor: STATE_BORDER_COLORS[state] }}
          />
        )}
        <span className="text-[11px] text-[#c0caf5] font-medium truncate flex-1">{tab.title}</span>
        {/* Filter output button */}
        {lastLines.length > 0 && (
          <button
            className={`p-0.5 rounded transition-colors shrink-0 ${
              showFilter || outputFilter
                ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
                : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]"
            }`}
            title={outputFilter ? `Filter: ${outputFilter}` : "Filter output by regex"}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => {
              e.stopPropagation();
              setShowFilter((prev) => {
                if (prev) setOutputFilter("");
                return !prev;
              });
            }}
          >
            <Filter className="w-2.5 h-2.5" />
          </button>
        )}
        {isStale && (
          <span
            className="flex items-center gap-0.5 text-[9px] text-[#e0af68] shrink-0"
            title="No output for 60s+ — may be stalled"
          >
            <AlertTriangle className="w-3 h-3" />
            stalled?
          </span>
        )}
        {activity && activity.length > 1 && <ActivitySparkline data={activity} />}
        {(() => {
          const trend = computeOutputTrend(activity);
          if (!trend || state !== "working") return null;
          const icon = TREND_ICONS[trend.trend];
          return (
            <span
              className="text-[8px] shrink-0"
              style={{ color: icon.color }}
              title={`Output trend: ${trend.trend}`}
            >
              {icon.symbol}
            </span>
          );
        })()}
        <span
          className={`text-[9px] px-1.5 py-0.5 rounded-full font-medium shrink-0 ${STATE_BG_COLORS[state]}`}
          style={{ color: STATE_BORDER_COLORS[state] }}
        >
          {STATE_LABELS[state]}
          {duration && <span className="ml-1 opacity-70">{duration}</span>}
          {uptime && <span className="ml-1 opacity-50">⏱{uptime}</span>}
        </span>
      </div>

      {/* Zone group label */}
      {(zoneLabel || onSetZoneLabel) && (
        <div className="shrink-0">
          {editingLabel ? (
            <input
              autoFocus
              value={labelValue}
              onChange={(e) => setLabelValue(e.target.value)}
              onBlur={() => {
                setEditingLabel(false);
                onSetZoneLabel?.(labelValue.trim());
              }}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === "Enter") {
                  setEditingLabel(false);
                  onSetZoneLabel?.(labelValue.trim());
                }
                if (e.key === "Escape") {
                  setEditingLabel(false);
                  setLabelValue(zoneLabel ?? "");
                }
              }}
              onMouseDown={(e) => e.stopPropagation()}
              onClick={(e) => e.stopPropagation()}
              className="bg-transparent border-b border-[#565f89] text-[9px] text-[#bb9af7] outline-hidden w-20"
              placeholder="Group label..."
            />
          ) : zoneLabel ? (
            <span
              className="text-[9px] px-1.5 py-0.5 rounded cursor-pointer transition-colors"
              style={{
                color: groupColor ?? "#bb9af7",
                backgroundColor: `${groupColor ?? "#bb9af7"}15`,
              }}
              onDoubleClick={(e) => {
                e.stopPropagation();
                setLabelValue(zoneLabel);
                setEditingLabel(true);
              }}
              title="Double-click to edit group label"
            >
              {zoneLabel}
            </span>
          ) : (
            <span
              className="text-[9px] text-[#565f89]/40 cursor-pointer hover:text-[#565f89] transition-colors"
              onDoubleClick={(e) => {
                e.stopPropagation();
                setLabelValue("");
                setEditingLabel(true);
              }}
              title="Double-click to add group label"
            >
              + label
            </span>
          )}
        </div>
      )}

      {/* Zone note */}
      {(note || onSetNote) && (
        <div className="shrink-0">
          {editingNote ? (
            <input
              autoFocus
              value={noteValue}
              onChange={(e) => setNoteValue(e.target.value.slice(0, 100))}
              onBlur={() => {
                setEditingNote(false);
                onSetNote?.(noteValue.trim());
              }}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === "Enter") {
                  setEditingNote(false);
                  onSetNote?.(noteValue.trim());
                }
                if (e.key === "Escape") {
                  setEditingNote(false);
                  setNoteValue(note ?? "");
                }
              }}
              onMouseDown={(e) => e.stopPropagation()}
              onClick={(e) => e.stopPropagation()}
              className="bg-transparent border-b border-[#565f89] text-[9px] text-[#a9b1d6] outline-hidden w-full"
              placeholder="Add a note..."
              maxLength={100}
            />
          ) : note ? (
            <span
              className="text-[9px] text-[#565f89] italic truncate block cursor-pointer hover:text-[#a9b1d6] transition-colors"
              onDoubleClick={(e) => {
                e.stopPropagation();
                setNoteValue(note);
                setEditingNote(true);
              }}
              title={`${note}\n(double-click to edit)`}
            >
              {note}
            </span>
          ) : (
            <span
              className="text-[9px] text-[#565f89]/30 cursor-pointer hover:text-[#565f89] transition-colors"
              onDoubleClick={(e) => {
                e.stopPropagation();
                setNoteValue("");
                setEditingNote(true);
              }}
              title="Double-click to add note"
            >
              + note
            </span>
          )}
        </div>
      )}

      {/* Hover preview overlay — shows more output lines */}
      {isHovered && lastLines.length > 6 && (
        <div
          className="absolute left-0 right-0 bottom-full mb-1 z-40 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-2xl p-2 max-h-[240px] overflow-y-auto scrollbar-dark"
          onMouseEnter={() => setIsHovered(true)}
          onMouseLeave={handleMouseLeave}
        >
          <div className="text-[9px] text-[#565f89] mb-1 font-medium">
            Output preview — {lastLines.length} lines
          </div>
          <div className="font-mono text-[10px] leading-relaxed">
            {lastLines.map((line, i) => {
              const actionable = needsInput && isActionableLine(line);
              return (
                <div
                  key={`preview-line-${i}`}
                  className={`truncate ${
                    actionable
                      ? "text-[#e0af68] font-semibold bg-[#e0af68]/10 rounded px-0.5 -mx-0.5"
                      : "text-[#a9b1d6]/70"
                  }`}
                >
                  {line ? <HighlightedText text={line} query={searchQuery} /> : "\u00A0"}
                </div>
              );
            })}
          </div>
        </div>
      )}

      {/* Working directory + last command */}
      {(tab.workingDir || lastCommand) && (
        <div className="shrink-0">
          {tab.workingDir && (
            <div className="text-[9px] text-[#565f89] truncate">
              {tab.workingDir.split(/[/\\]/).slice(-2).join("/")}
            </div>
          )}
          {lastCommand && (
            <div className="text-[9px] text-[#7aa2f7]/50 font-mono truncate" title={lastCommand}>
              $ {lastCommand}
            </div>
          )}
        </div>
      )}

      {/* Output filter input */}
      {showFilter && (
        <div className="flex items-center gap-1 shrink-0">
          <input
            autoFocus
            value={outputFilter}
            onChange={(e) => setOutputFilter(e.target.value)}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === "Escape") {
                setShowFilter(false);
                setOutputFilter("");
              }
            }}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => e.stopPropagation()}
            placeholder="Filter regex..."
            className="flex-1 bg-[#13141f] border border-[#2a2d3d] rounded px-1.5 py-0.5 text-[9px] font-mono text-[#c0caf5] placeholder-[#565f89] outline-hidden focus:border-[#7aa2f7] transition-colors"
          />
          {outputFilter &&
            (() => {
              let matchCount = 0;
              try {
                const re = new RegExp(outputFilter, "i");
                matchCount = lastLines.filter((l) => re.test(l)).length;
              } catch {
                // intentionally empty
              }
              return (
                <span
                  className={`text-[9px] shrink-0 ${matchCount > 0 ? "text-[#9ece6a]" : "text-[#565f89]"}`}
                >
                  {matchCount}/{lastLines.length}
                </span>
              );
            })()}
        </div>
      )}

      {/* Last output lines — scrollable, highlight actionable prompts + search */}
      <div
        role="presentation"
        ref={outputRef}
        className="flex-1 min-h-0 overflow-y-auto scrollbar-dark"
        onMouseDown={(e) => e.stopPropagation()}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="font-mono text-[10px] leading-relaxed">
          {(() => {
            let filteredLines = lastLines;
            let filterRegex: RegExp | null = null;
            if (outputFilter) {
              try {
                filterRegex = new RegExp(outputFilter, "i");
                filteredLines = lastLines.filter((l) => filterRegex!.test(l));
              } catch {
                // Invalid regex — show all lines
              }
            }
            return filteredLines.length > 0 ? (
              filteredLines.map((line, i) => {
                const actionable = needsInput && isActionableLine(line);
                return (
                  <div
                    key={`output-line-${i}`}
                    className={`truncate ${
                      actionable
                        ? "text-[#e0af68] font-semibold bg-[#e0af68]/10 rounded px-0.5 -mx-0.5"
                        : "text-[#a9b1d6]/70"
                    }`}
                  >
                    {line ? (
                      <HighlightedText
                        text={line}
                        query={filterRegex ? outputFilter : searchQuery}
                      />
                    ) : (
                      "\u00A0"
                    )}
                  </div>
                );
              })
            ) : (
              <div className="text-[#565f89] italic">
                {outputFilter ? "No matching lines" : "No output yet"}
              </div>
            );
          })()}
        </div>
      </div>

      {/* Quick actions when waiting for input */}
      {needsInput && onQuickApprove ? (
        <div className="flex flex-col gap-1 shrink-0">
          {/* Yes/No buttons for y/n prompts */}
          {yesNo && (
            <div className="flex items-center gap-1.5">
              <button
                className="flex-1 flex items-center justify-center gap-1 text-[10px] font-medium
                  py-1 rounded bg-[#9ece6a]/15 text-[#9ece6a] hover:bg-[#9ece6a]/25
                  transition-colors"
                onMouseDown={(e) => e.stopPropagation()}
                onClick={(e) => {
                  e.stopPropagation();
                  onQuickApprove();
                }}
              >
                <Check className="w-3 h-3" />
                Approve
              </button>
              {onQuickReject && (
                <button
                  className="flex-1 flex items-center justify-center gap-1 text-[10px] font-medium
                    py-1 rounded bg-[#f7768e]/15 text-[#f7768e] hover:bg-[#f7768e]/25
                    transition-colors"
                  onMouseDown={(e) => e.stopPropagation()}
                  onClick={(e) => {
                    e.stopPropagation();
                    onQuickReject();
                  }}
                >
                  <X className="w-3 h-3" />
                  Reject
                </button>
              )}
            </div>
          )}
          {/* Text input for free-text prompts (always shown, more prominent when not y/n) */}
          {onSendCommand && (
            <div className="flex items-center gap-1">
              <input
                type="text"
                value={commandText}
                onChange={(e) => setCommandText(e.target.value)}
                onMouseDown={(e) => e.stopPropagation()}
                onClick={(e) => e.stopPropagation()}
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Enter" && commandText.trim()) {
                    onSendCommand(commandText);
                    setCommandText("");
                  }
                }}
                placeholder={yesNo ? "or type..." : "Type response..."}
                className={`flex-1 min-w-0 bg-[#2a2d3d]/50 border border-[#3b3d57] rounded px-1.5 py-0.5
                  text-[10px] text-[#c0caf5] placeholder-[#565f89] outline-hidden
                  focus:border-[#7aa2f7] transition-colors ${!yesNo ? "border-[#e0af68]/50" : ""}`}
              />
              <button
                onMouseDown={(e) => e.stopPropagation()}
                onClick={(e) => {
                  e.stopPropagation();
                  if (commandText.trim()) {
                    onSendCommand(commandText);
                    setCommandText("");
                  }
                }}
                disabled={!commandText.trim()}
                className="p-1 rounded text-[#565f89] hover:text-[#7aa2f7] disabled:opacity-30 transition-colors"
              >
                <Send className="w-3 h-3" />
              </button>
            </div>
          )}
        </div>
      ) : (state === "completed" || state === "error") && onRestart ? (
        <div className="flex items-center gap-1.5 shrink-0">
          <button
            className="flex-1 flex items-center justify-center gap-1 text-[10px] font-medium
              py-1 rounded bg-[#7aa2f7]/15 text-[#7aa2f7] hover:bg-[#7aa2f7]/25
              transition-colors"
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => {
              e.stopPropagation();
              onRestart();
            }}
          >
            <RotateCcw className="w-3 h-3" />
            Restart
          </button>
        </div>
      ) : (
        <div className="flex items-center gap-1 text-[9px] text-[#565f89]/60 shrink-0">
          <Maximize2 className="w-2.5 h-2.5" />
          <span>Click to focus, double-click to maximize</span>
        </div>
      )}

      {/* Activity progress bar for working sessions */}
      {state === "working" &&
        (() => {
          const trend = computeOutputTrend(activity);
          if (!trend || trend.peak === 0) return null;
          const pct = Math.min(100, (trend.rate / trend.peak) * 100);
          return (
            <div className="absolute bottom-0 left-0 right-0 h-[2px] bg-[#2a2d3d]/50">
              <div
                className="h-full transition-all duration-1000 ease-out"
                style={{
                  width: `${pct}%`,
                  backgroundColor: TREND_ICONS[trend.trend].color,
                  opacity: 0.6,
                }}
              />
            </div>
          );
        })()}
    </div>
  );
}

/** Reusable menu item for ZoneContextMenu */
function ZoneMenuItem({
  label,
  onClick,
  onClose,
  danger,
  disabled,
}: {
  label: string;
  onClick: () => void;
  onClose: () => void;
  danger?: boolean;
  disabled?: boolean;
}) {
  return (
    <button
      onClick={() => {
        onClick();
        onClose();
      }}
      disabled={disabled}
      className={`w-full text-left px-3 py-1.5 text-[11px] transition-colors disabled:opacity-30 ${
        danger ? "text-[#f7768e] hover:bg-[#f7768e]/10" : "text-[#c0caf5] hover:bg-[#7aa2f7]/10"
      }`}
    >
      {label}
    </button>
  );
}

/** Context menu for zone right-click actions */
function ZoneContextMenu({
  x,
  y,
  zoneIndex,
  tab,
  state,
  otherZones,
  onClose,
  onFocus,
  onMaximize,
  onApprove,
  onReject,
  onSwap,
  onUnassign,
  onRestart,
}: {
  x: number;
  y: number;
  zoneIndex: number;
  tab: TerminalTab | undefined;
  state: SessionState;
  otherZones: { index: number; title: string }[];
  onClose: () => void;
  onFocus: () => void;
  onMaximize: () => void;
  onApprove: () => void;
  onReject: () => void;
  onSwap: (targetZone: number) => void;
  onUnassign: () => void;
  onRestart?: () => void;
}) {
  const menuRef = useRef<HTMLDivElement>(null);
  const [showSwapSub, setShowSwapSub] = useState(false);

  useEffect(() => {
    const handleClick = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        onClose();
      }
    };
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("mousedown", handleClick);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleClick);
      document.removeEventListener("keydown", handleKey);
    };
  }, [onClose]);

  const needsInput = state === "needs-input";

  return (
    <div
      ref={menuRef}
      className="fixed z-50 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl py-1 min-w-[160px] overflow-hidden"
      style={{
        left: x,
        top: y,
      }}
    >
      <div className="px-3 py-1 text-[9px] text-[#565f89] uppercase tracking-wider border-b border-[#2a2d3d] mb-1">
        Zone {zoneIndex + 1}
        {tab ? ` — ${tab.title}` : ""}
      </div>

      <ZoneMenuItem label="Focus" onClick={onFocus} onClose={onClose} />
      <ZoneMenuItem label="Maximize" onClick={onMaximize} onClose={onClose} disabled={!tab} />

      {needsInput && (
        <>
          <div className="h-px bg-[#2a2d3d] my-1" />
          <ZoneMenuItem label="Approve (y)" onClick={onApprove} onClose={onClose} />
          <ZoneMenuItem label="Reject (n)" onClick={onReject} onClose={onClose} />
        </>
      )}

      {(state === "completed" || state === "error") && onRestart && (
        <>
          <div className="h-px bg-[#2a2d3d] my-1" />
          <ZoneMenuItem label="Restart session" onClick={onRestart} onClose={onClose} />
        </>
      )}

      {otherZones.length > 0 && tab && (
        <>
          <div className="h-px bg-[#2a2d3d] my-1" />
          <div className="relative">
            <button
              className="w-full text-left px-3 py-1.5 text-[11px] text-[#c0caf5] hover:bg-[#7aa2f7]/10 flex items-center justify-between"
              onClick={() => setShowSwapSub(!showSwapSub)}
            >
              Swap with...
              <ChevronDown
                className={`w-3 h-3 transition-transform ${showSwapSub ? "rotate-180" : ""}`}
              />
            </button>
            {showSwapSub && (
              <div className="bg-[#13141f] border-t border-[#2a2d3d]">
                {otherZones.map((z) => (
                  <button
                    key={z.index}
                    className="w-full text-left px-5 py-1 text-[10px] text-[#a9b1d6] hover:bg-[#7aa2f7]/10"
                    onClick={() => {
                      onSwap(z.index);
                      onClose();
                    }}
                  >
                    Zone {z.index + 1}: {z.title}
                  </button>
                ))}
              </div>
            )}
          </div>
        </>
      )}

      {tab && (
        <>
          <div className="h-px bg-[#2a2d3d] my-1" />
          <ZoneMenuItem label="Unassign from zone" onClick={onUnassign} onClose={onClose} danger />
        </>
      )}
    </div>
  );
}

/** Hidden terminal wrapper — keeps the PTY alive without rendering xterm visible */
function HiddenTerminal({
  tab,
  terminalRef,
  onExit,
  onFirstInput,
  onShellIntegration,
  onOutput,
  onReconnected,
}: {
  tab: TerminalTab;
  terminalRef: RefObject<TerminalInstanceHandle | null> | undefined;
  onExit: (terminalId: string, exitCode: number | null) => void;
  onFirstInput: (terminalId: string, input: string) => void;
  onShellIntegration: (tabId: string, event: ShellIntegrationEvent) => void;
  onOutput: (tabId: string, text: string) => void;
  onReconnected: (tabId: string) => void;
}) {
  return (
    <div className="hidden">
      <TerminalInstance
        ref={terminalRef}
        terminalId={tab.id}
        visible={false}
        isReconnecting={tab.isReconnecting}
        onReconnected={() => onReconnected(tab.id)}
        onExit={(code) => onExit(tab.id, code)}
        onFirstInput={(input) => onFirstInput(tab.id, input)}
        onShellIntegration={(event) => onShellIntegration(tab.id, event)}
        onOutput={(text) => onOutput(tab.id, text)}
      />
    </div>
  );
}

/** Floating quick-action toolbar for zones in full terminal view */
function ZoneQuickActions({
  zoneIndex: _zoneIndex,
  isPinned,
  onTogglePin,
  onMaximize,
  onCopyOutput,
  onScrollToBottom,
  lastLines,
  state,
  onRestart,
  onExportZone,
}: {
  zoneIndex: number;
  isPinned?: boolean;
  onTogglePin?: () => void;
  onMaximize: () => void;
  onCopyOutput: () => void;
  onScrollToBottom?: () => void;
  lastLines: string[];
  state?: SessionState;
  onRestart?: () => void;
  onExportZone?: (format: "text" | "markdown" | "json") => void;
}) {
  const [visible, setVisible] = useState(false);
  const [copied, setCopied] = useState(false);
  const [exportOpen, setExportOpen] = useState(false);
  const exportRef = useRef<HTMLDivElement>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Close export dropdown on outside click
  useEffect(() => {
    if (!exportOpen) return;
    const handler = (e: MouseEvent) => {
      if (exportRef.current && !exportRef.current.contains(e.target as Node)) {
        setExportOpen(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [exportOpen]);

  const show = useCallback(() => {
    timerRef.current = setTimeout(() => setVisible(true), 500);
  }, []);
  const hide = useCallback(() => {
    if (timerRef.current) clearTimeout(timerRef.current);
    setVisible(false);
    setExportOpen(false);
  }, []);

  if (!visible) {
    return (
      <div
        className="absolute inset-0 z-[5]"
        onMouseEnter={show}
        onMouseLeave={hide}
        style={{ pointerEvents: "none" }}
      />
    );
  }

  return (
    <>
      <div
        className="absolute inset-0 z-[5]"
        onMouseLeave={hide}
        style={{ pointerEvents: "none" }}
      />
      <div
        className="absolute top-1 right-1 z-20 flex items-center gap-0.5 px-1 py-0.5 bg-[#1a1b26]/90 border border-[#2a2d3d] rounded-md shadow-lg backdrop-blur-sm"
        onMouseEnter={() => setVisible(true)}
        onMouseLeave={hide}
      >
        {onTogglePin && (
          <button
            onClick={(e) => {
              e.stopPropagation();
              onTogglePin();
            }}
            onMouseDown={(e) => e.stopPropagation()}
            className={`p-1 rounded transition-colors ${isPinned ? "text-[#7aa2f7]" : "text-[#565f89] hover:text-[#a9b1d6]"}`}
            title={isPinned ? "Unpin" : "Pin"}
          >
            <Pin className="w-3 h-3" />
          </button>
        )}
        <button
          onClick={(e) => {
            e.stopPropagation();
            onMaximize();
          }}
          onMouseDown={(e) => e.stopPropagation()}
          className="p-1 rounded text-[#565f89] hover:text-[#a9b1d6] transition-colors"
          title="Maximize"
        >
          <Maximize2 className="w-3 h-3" />
        </button>
        <button
          onClick={(e) => {
            e.stopPropagation();
            onCopyOutput();
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
          }}
          onMouseDown={(e) => e.stopPropagation()}
          className={`p-1 rounded transition-colors ${copied ? "text-[#9ece6a]" : "text-[#565f89] hover:text-[#a9b1d6]"}`}
          title={copied ? "Copied!" : "Copy output"}
          disabled={lastLines.length === 0}
        >
          <Copy className="w-3 h-3" />
        </button>
        {onScrollToBottom && (
          <button
            onClick={(e) => {
              e.stopPropagation();
              onScrollToBottom();
            }}
            onMouseDown={(e) => e.stopPropagation()}
            className="p-1 rounded text-[#565f89] hover:text-[#a9b1d6] transition-colors"
            title="Scroll to bottom"
          >
            <ArrowDownToLine className="w-3 h-3" />
          </button>
        )}
        {(state === "completed" || state === "error") && onRestart && (
          <button
            onClick={(e) => {
              e.stopPropagation();
              onRestart();
            }}
            onMouseDown={(e) => e.stopPropagation()}
            className="p-1 rounded text-[#7aa2f7] hover:text-[#89b4fa] transition-colors"
            title="Restart session"
          >
            <RotateCcw className="w-3 h-3" />
          </button>
        )}
        {onExportZone && (
          <div ref={exportRef} className="relative">
            <button
              onClick={(e) => {
                e.stopPropagation();
                setExportOpen((v) => !v);
              }}
              onMouseDown={(e) => e.stopPropagation()}
              className="p-1 rounded text-[#565f89] hover:text-[#a9b1d6] transition-colors"
              title="Export output"
              disabled={lastLines.length === 0}
            >
              <Download className="w-3 h-3" />
            </button>
            {exportOpen && (
              <div className="absolute top-full right-0 mt-1 z-50 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl min-w-[120px] py-1">
                {(
                  [
                    ["text", "Plain text"],
                    ["markdown", "Markdown"],
                    ["json", "JSON"],
                  ] as const
                ).map(([fmt, label]) => (
                  <button
                    key={fmt}
                    onClick={(e) => {
                      e.stopPropagation();
                      onExportZone(fmt);
                      setExportOpen(false);
                    }}
                    onMouseDown={(e) => e.stopPropagation()}
                    className="w-full text-left px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
                  >
                    {label}
                  </button>
                ))}
              </div>
            )}
          </div>
        )}
      </div>
    </>
  );
}

export function ZoneGrid({
  layout,
  assignments,
  tabs,
  focusedZone,
  maximizedZone,
  sessionStates,
  lastOutputLines,
  viewMode,
  terminalRefs,
  onZoneClick,
  onZoneDoubleClick,
  onExit,
  onFirstInput,
  onShellIntegration,
  onOutput,
  onReconnected,
  onAssignTab,
  flashingTabs,
  stateDurations,
  selectedZones,
  staleTabs,
  pinnedZones,
  onTogglePin,
  outputSearchQuery,
  swapSource,
  activityData,
  zoneLabels,
  onSetZoneLabel,
  onRestartInZone,
  resetRatiosKey,
  labelColorMap,
  zoneTags,
  commandHistories,
  focusMode,
  zoneNotes,
  onSetZoneNote,
  onExportZone,
  pendingRestarts,
  onCancelRestart,
}: ZoneGridProps) {
  const [dropTargetZone, setDropTargetZone] = useState<number | null>(null);
  const [contextMenu, setContextMenu] = useState<{
    x: number;
    y: number;
    zoneIndex: number;
  } | null>(null);
  // Per-zone output filter state
  const [zoneFilters, setZoneFilters] = useState<Record<number, string>>({});
  const [showFilterInput, setShowFilterInput] = useState<number | null>(null);
  // Custom column/row ratios for drag-to-resize (persisted per layout)
  const [colRatios, setColRatios] = useState<number[]>(() => {
    const parsed = instanceStorage.getJSON<number[]>(`zone-col-ratios-${layout.id}`, []);
    if (parsed.length === layout.columns) return parsed;
    return Array(layout.columns).fill(1);
  });
  const [rowRatios, setRowRatios] = useState<number[]>(() => {
    const parsed = instanceStorage.getJSON<number[]>(`zone-row-ratios-${layout.id}`, []);
    if (parsed.length === layout.rows) return parsed;
    return Array(layout.rows).fill(1);
  });
  const gridRef = useRef<HTMLDivElement>(null);
  const prevLayoutIdRef = useRef(layout.id);

  // Persist ratios when they change
  useEffect(() => {
    instanceStorage.setJSON(`zone-col-ratios-${layout.id}`, colRatios);
  }, [colRatios, layout.id]);
  useEffect(() => {
    instanceStorage.setJSON(`zone-row-ratios-${layout.id}`, rowRatios);
  }, [rowRatios, layout.id]);

  // Load or reset ratios when layout changes
  useEffect(() => {
    if (prevLayoutIdRef.current !== layout.id) {
      prevLayoutIdRef.current = layout.id;
      const parsedCols = instanceStorage.getJSON<number[]>(`zone-col-ratios-${layout.id}`, []);
      if (parsedCols.length === layout.columns) {
        setColRatios(parsedCols);
      } else {
        setColRatios(Array(layout.columns).fill(1));
      }
      const parsedRows = instanceStorage.getJSON<number[]>(`zone-row-ratios-${layout.id}`, []);
      if (parsedRows.length === layout.rows) {
        setRowRatios(parsedRows);
      } else {
        setRowRatios(Array(layout.rows).fill(1));
      }
    }
  }, [layout.id, layout.columns, layout.rows]);

  // Reset ratios when resetRatiosKey changes (from external Reset button)
  const prevResetKeyRef = useRef(resetRatiosKey);
  useEffect(() => {
    if (resetRatiosKey !== undefined && resetRatiosKey !== prevResetKeyRef.current) {
      prevResetKeyRef.current = resetRatiosKey;
      setColRatios(Array(layout.columns).fill(1));
      setRowRatios(Array(layout.rows).fill(1));
    }
  }, [resetRatiosKey, layout.columns, layout.rows]);

  // Tick counter to force uptime re-render every 30s
  const [, setUptimeTick] = useState(0);
  useEffect(() => {
    const interval = setInterval(() => setUptimeTick((t) => t + 1), 30000);
    return () => clearInterval(interval);
  }, []);

  const isMultiZone = layout.zones.length > 1;
  const showLabels = isMultiZone;
  // Auto-compact disabled: all zones show full xterm terminals in "auto" mode.
  // Users can opt into compact mode with Ctrl+Shift+M (viewMode → "compact").
  const autoCompact = false;
  const forceCompact = viewMode === "compact";

  // In single layout or maximized mode, render just one zone full-size
  const isSingleView = layout.id === "single" || maximizedZone !== null;
  const singleViewZone = maximizedZone ?? 0;

  const handleZoneMouseDown = useCallback(
    (zoneIndex: number, e: React.MouseEvent) => {
      onZoneClick(zoneIndex, e.ctrlKey || e.metaKey);
    },
    [onZoneClick],
  );

  // Unassigned tabs (hidden but alive) — exclude plan tabs (no PTY)
  const unassignedTabs = tabs.filter((t) => !Object.values(assignments).includes(t.id));
  const unassignedTerminals = unassignedTabs.filter((t) => t.type !== "plan");

  const renderHiddenTabs = (extraTabs: TerminalTab[]) =>
    extraTabs.map((tab) => (
      <HiddenTerminal
        key={tab.id}
        tab={tab}
        terminalRef={terminalRefs.get(tab.id)}
        onExit={onExit}
        onFirstInput={onFirstInput}
        onShellIntegration={onShellIntegration}
        onOutput={onOutput}
        onReconnected={onReconnected}
      />
    ));

  if (isSingleView && layout.id !== "single") {
    // Maximized mode: show the maximized zone full-screen, keep others mounted but hidden
    const tabId = assignments[singleViewZone];
    const tab = tabs.find((t) => t.id === tabId);

    return (
      <div className="h-full w-full relative">
        {/* Maximized zone header */}
        {tab && (
          <div className="absolute top-0 left-0 right-0 flex items-center gap-2 px-3 py-1 bg-[#13141f]/90 backdrop-blur-sm z-20">
            <span className="text-[10px] text-[#7aa2f7] font-medium">Maximized</span>
            <span className="text-[10px] text-[#a9b1d6]">{tab.title}</span>
            <span className="text-[9px] text-[#565f89] ml-auto">
              Esc or double-click to restore
            </span>
          </div>
        )}

        {/* Render all terminals, only maximized one visible */}
        {layout.zones.map((_, zoneIdx) => {
          const zoneTabId = assignments[zoneIdx];
          const zoneTab = tabs.find((t) => t.id === zoneTabId);
          if (!zoneTab) return null;

          const isVisible = zoneIdx === singleViewZone;
          const ref = terminalRefs.get(zoneTab.id);

          return (
            <div
              key={zoneTab.id}
              className={isVisible ? "h-full w-full" : "hidden"}
              onMouseDown={(e) => handleZoneMouseDown(zoneIdx, e)}
              onDoubleClick={() => onZoneDoubleClick(zoneIdx)}
            >
              {zoneTab.type === "plan" && zoneTab.planFilePath ? (
                <PlanViewer filePath={zoneTab.planFilePath} visible={isVisible} />
              ) : (
                <TerminalInstance
                  ref={ref}
                  terminalId={zoneTab.id}
                  visible={isVisible}
                  isReconnecting={zoneTab.isReconnecting}
                  onReconnected={() => onReconnected(zoneTab.id)}
                  onExit={(code) => onExit(zoneTab.id, code)}
                  onFirstInput={(input) => onFirstInput(zoneTab.id, input)}
                  onShellIntegration={(event) => onShellIntegration(zoneTab.id, event)}
                  onOutput={(text) => onOutput(zoneTab.id, text)}
                />
              )}
            </div>
          );
        })}

        {renderHiddenTabs(unassignedTerminals)}
      </div>
    );
  }

  return (
    <div
      ref={gridRef}
      className="h-full w-full relative"
      style={{
        display: "grid",
        gridTemplateColumns: colRatios.map((r) => `${r}fr`).join(" "),
        gridTemplateRows: rowRatios.map((r) => `${r}fr`).join(" "),
        gap: "2px",
      }}
      onDragEnd={() => setDropTargetZone(null)}
    >
      {layout.zones.map((zone, zoneIdx) => {
        const tabId = assignments[zoneIdx];
        const tab = tabs.find((t) => t.id === tabId);
        const isFocused = zoneIdx === focusedZone;
        const state = tab ? (sessionStates[tab.id] ?? "idle") : "idle";
        const borderColor = isFocused
          ? STATE_BORDER_COLORS[state] === "#2a2d3d"
            ? "#7aa2f7"
            : STATE_BORDER_COLORS[state]
          : STATE_BORDER_COLORS[state];

        // Decide: compact or full terminal for this zone (pinned zones stay full)
        const isPinned = pinnedZones?.has(zoneIdx);
        const useCompact =
          tab && tab.type !== "plan" && !isFocused && !isPinned && (autoCompact || forceCompact);
        const isFlashing = tab && flashingTabs?.has(tab.id);
        const isStale = tab && staleTabs?.has(tab.id);
        const searchMatch =
          tab && outputSearchQuery
            ? (lastOutputLines[tab.id] ?? []).some((l) =>
                l.toLowerCase().includes(outputSearchQuery.toLowerCase()),
              )
            : false;
        const isSwapSource = swapSource === zoneIdx;
        const isSelected = selectedZones?.has(zoneIdx);

        const isDropTarget = dropTargetZone === zoneIdx;
        const firstTagColor = zoneTags?.[zoneIdx]?.[0]
          ? labelColorMap?.[zoneTags[zoneIdx][0]]
          : undefined;
        const stateColor = STATE_COLORS[state];

        // Compute base boxShadow from interaction/focus state
        const baseBoxShadow = isDropTarget
          ? "inset 0 0 16px rgba(122, 162, 247, 0.15), 0 0 8px rgba(122, 162, 247, 0.3)"
          : isSwapSource
            ? "0 0 10px rgba(255, 158, 100, 0.5), inset 0 0 6px rgba(255, 158, 100, 0.1)"
            : isFlashing
              ? "0 0 12px rgba(224, 175, 104, 0.6), inset 0 0 8px rgba(224, 175, 104, 0.15)"
              : searchMatch
                ? "0 0 6px rgba(158, 206, 106, 0.4)"
                : isSelected
                  ? "0 0 6px rgba(187, 154, 247, 0.3)"
                  : isFocused
                    ? STATE_GLOW[state]
                    : "none";

        // For needs-input, add inset glow on the left edge when no higher-priority shadow is active
        const zoneShadow =
          state === "needs-input" && baseBoxShadow === "none"
            ? `inset 3px 0 8px -4px ${stateColor}40`
            : baseBoxShadow;

        return (
          <div
            key={zoneIdx}
            className={`relative overflow-hidden ${isFlashing ? "zone-flash" : ""} ${
              outputSearchQuery && !searchMatch ? "opacity-40" : ""
            }`}
            style={{
              gridColumn: zone.col,
              gridRow: zone.row,
              border: `${isFocused ? "2px" : isSwapSource ? "2px" : isSelected ? "2px" : searchMatch ? "2px" : "1px"} solid ${
                isDropTarget
                  ? "#7aa2f7"
                  : isSwapSource
                    ? "#ff9e64"
                    : isSelected
                      ? "#bb9af7"
                      : searchMatch
                        ? "#9ece6a"
                        : isStale
                          ? "#e0af68"
                          : borderColor
              }`,
              borderLeft: `${state === "needs-input" ? "3px" : "2px"} solid ${stateColor}`,
              borderStyle: isSwapSource
                ? "dashed"
                : isStale && !isFocused && !searchMatch
                  ? "dashed"
                  : "solid",
              borderLeftStyle: "solid",
              borderRadius: "4px",
              boxShadow: zoneShadow,
              transition: "border-color 0.2s, box-shadow 0.2s, opacity 0.3s",
              opacity:
                focusMode && !isFocused && state !== "needs-input" && state !== "error" ? 0.3 : 1,
              animation: isFlashing ? "zone-flash-border 1s ease-out" : undefined,
              ...(firstTagColor
                ? { background: `linear-gradient(135deg, ${firstTagColor}08 0%, transparent 60%)` }
                : {}),
            }}
            onMouseDown={(e) => handleZoneMouseDown(zoneIdx, e)}
            onDoubleClick={() => onZoneDoubleClick(zoneIdx)}
            onContextMenu={(e) => {
              e.preventDefault();
              setContextMenu({ x: e.clientX, y: e.clientY, zoneIndex: zoneIdx });
            }}
            onDragOver={(e) => {
              if (
                e.dataTransfer.types.includes("text/tab-id") ||
                e.dataTransfer.types.includes("text/zone-index")
              ) {
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                setDropTargetZone(zoneIdx);
              }
            }}
            onDragLeave={(e) => {
              // Only clear if leaving this zone (not entering a child)
              if (!e.currentTarget.contains(e.relatedTarget as Node)) {
                setDropTargetZone(null);
              }
            }}
            onDrop={(e) => {
              e.preventDefault();
              setDropTargetZone(null);
              const droppedTabId = e.dataTransfer.getData("text/tab-id");
              if (droppedTabId && onAssignTab) {
                onAssignTab(zoneIdx, droppedTabId);
                return;
              }
              // Zone-to-zone swap
              const srcZoneStr = e.dataTransfer.getData("text/zone-index");
              if (srcZoneStr && onAssignTab) {
                const srcZone = Number(srcZoneStr);
                if (srcZone !== zoneIdx) {
                  const srcTabId = assignments[srcZone];
                  const dstTabId = assignments[zoneIdx];
                  if (srcTabId) onAssignTab(zoneIdx, srcTabId);
                  if (dstTabId) onAssignTab(srcZone, dstTabId);
                  // Swap labels and notes
                  if (onSetZoneLabel) {
                    const srcLabel = zoneLabels?.[srcZone] ?? "";
                    const dstLabel = zoneLabels?.[zoneIdx] ?? "";
                    onSetZoneLabel(zoneIdx, srcLabel);
                    onSetZoneLabel(srcZone, dstLabel);
                  }
                  if (onSetZoneNote) {
                    const srcNote = zoneNotes?.[srcZone] ?? "";
                    const dstNote = zoneNotes?.[zoneIdx] ?? "";
                    onSetZoneNote(zoneIdx, srcNote);
                    onSetZoneNote(srcZone, dstNote);
                  }
                }
              }
            }}
          >
            {/* Group color accent strip */}
            {(() => {
              const label = zoneLabels?.[zoneIdx];
              const groupColor = label && labelColorMap?.[label];
              return groupColor ? (
                <div
                  className="absolute left-0 top-0 bottom-0 w-[3px] z-10 rounded-l"
                  style={{ backgroundColor: groupColor, opacity: 0.6 }}
                />
              ) : null;
            })()}

            {/* Auto-restart countdown overlay */}
            {pendingRestarts?.[zoneIdx] && (
              <div className="absolute inset-0 z-20 flex items-center justify-center bg-black/40 rounded">
                <div className="flex items-center gap-2 bg-[#1a1b26] border border-[#7dcfff]/30 rounded-lg px-3 py-1.5">
                  <RefreshCw className="w-3 h-3 text-[#7dcfff] animate-spin" />
                  <span className="text-[10px] text-[#7dcfff]">Restarting...</span>
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      onCancelRestart?.(zoneIdx);
                    }}
                    className="text-[9px] text-[#f7768e] hover:text-[#ff9e9e] px-1.5 py-0.5 rounded bg-[#f7768e]/10 hover:bg-[#f7768e]/20 transition-colors"
                  >
                    Cancel
                  </button>
                </div>
              </div>
            )}

            {tab ? (
              <>
                {/* Compact card overlay - shown when in compact mode */}
                {useCompact && (
                  <CompactZoneCard
                    tab={tab}
                    state={state}
                    zoneIndex={zoneIdx}
                    lastLines={lastOutputLines[tab.id] ?? []}
                    onQuickApprove={() => {
                      const ref = terminalRefs.get(tab.id);
                      ref?.current?.writeToTerminal("y\r");
                    }}
                    onQuickReject={() => {
                      const ref = terminalRefs.get(tab.id);
                      ref?.current?.writeToTerminal("n\r");
                    }}
                    onSendCommand={(text) => {
                      const ref = terminalRefs.get(tab.id);
                      ref?.current?.writeToTerminal(`${text}\r`);
                    }}
                    duration={stateDurations?.[tab.id]}
                    isStale={isStale ?? false}
                    searchQuery={outputSearchQuery}
                    activity={activityData?.[tab.id]}
                    zoneLabel={zoneLabels?.[zoneIdx]}
                    onSetZoneLabel={
                      onSetZoneLabel ? (label) => onSetZoneLabel(zoneIdx, label) : undefined
                    }
                    onRestart={onRestartInZone ? () => onRestartInZone(zoneIdx) : undefined}
                    groupColor={
                      zoneLabels?.[zoneIdx] ? labelColorMap?.[zoneLabels[zoneIdx]] : undefined
                    }
                    uptime={formatUptime(tab.createdAt)}
                    lastCommand={commandHistories?.[tab.id]?.slice(-1)[0]?.command}
                    note={zoneNotes?.[zoneIdx]}
                    onSetNote={onSetZoneNote ? (n) => onSetZoneNote(zoneIdx, n) : undefined}
                    allTabs={tabs}
                    assignments={assignments}
                    sessionStates={sessionStates}
                    onAssignTab={onAssignTab}
                    tagColor={firstTagColor}
                  />
                )}
                {/* Full terminal view elements - shown when not compact */}
                {!useCompact && showLabels && (
                  <ZoneLabel
                    tab={tab}
                    state={state}
                    zoneIndex={zoneIdx}
                    allTabs={tabs}
                    assignments={assignments}
                    sessionStates={sessionStates}
                    onAssignTab={onAssignTab}
                    isPinned={isPinned}
                    onTogglePin={onTogglePin ? () => onTogglePin(zoneIdx) : undefined}
                    zoneLabel={zoneLabels?.[zoneIdx]}
                    onSetZoneLabel={
                      onSetZoneLabel ? (label) => onSetZoneLabel(zoneIdx, label) : undefined
                    }
                    onScrollToBottom={() => {
                      const ref = terminalRefs.get(tab.id);
                      ref?.current?.scrollToBottom();
                    }}
                    outputLineCount={(lastOutputLines[tab.id] ?? []).length}
                    outputByteSize={(lastOutputLines[tab.id] ?? []).reduce(
                      (sum, l) => sum + l.length,
                      0,
                    )}
                    onToggleFilter={() =>
                      setShowFilterInput((prev) => (prev === zoneIdx ? null : zoneIdx))
                    }
                    filterActive={!!zoneFilters[zoneIdx]}
                  />
                )}
                {/* Per-zone filter input bar */}
                {!useCompact && showFilterInput === zoneIdx && (
                  <div
                    className="absolute left-0 right-0 flex items-center gap-2 px-2 py-1 bg-[#1a1b26] border-b border-[#2a2d3d] z-10"
                    style={{ top: showLabels ? "20px" : "0px" }}
                  >
                    <Filter className="w-3 h-3 text-[#565f89]" />
                    <input
                      autoFocus
                      value={zoneFilters[zoneIdx] ?? ""}
                      onChange={(e) =>
                        setZoneFilters((prev) => ({ ...prev, [zoneIdx]: e.target.value }))
                      }
                      onKeyDown={(e) => {
                        e.stopPropagation();
                        if (e.key === "Escape") {
                          setShowFilterInput(null);
                        }
                      }}
                      onMouseDown={(e) => e.stopPropagation()}
                      onClick={(e) => e.stopPropagation()}
                      placeholder="Filter output..."
                      className="flex-1 bg-transparent text-[10px] text-[#c0caf5] placeholder-[#565f89] outline-hidden font-mono"
                    />
                    {zoneFilters[zoneIdx] && (
                      <>
                        <span className="text-[9px] text-[#e0af68] font-mono">
                          {countMatches(lastOutputLines[tab.id] ?? [], zoneFilters[zoneIdx])}{" "}
                          matches
                        </span>
                        <button
                          onClick={() =>
                            setZoneFilters((prev) => {
                              const n = { ...prev };
                              delete n[zoneIdx];
                              return n;
                            })
                          }
                          onMouseDown={(e) => e.stopPropagation()}
                          className="text-[9px] text-[#565f89] hover:text-[#f7768e] px-1"
                        >
                          Clear
                        </button>
                      </>
                    )}
                  </div>
                )}
                {!useCompact && isMultiZone && (
                  <ZoneQuickActions
                    zoneIndex={zoneIdx}
                    isPinned={isPinned}
                    onTogglePin={onTogglePin ? () => onTogglePin(zoneIdx) : undefined}
                    onMaximize={() => onZoneDoubleClick(zoneIdx)}
                    onCopyOutput={() => {
                      const lines = lastOutputLines[tab.id] ?? [];
                      navigator.clipboard.writeText(lines.join("\n"));
                    }}
                    onScrollToBottom={() => {
                      const ref = terminalRefs.get(tab.id);
                      ref?.current?.scrollToBottom();
                    }}
                    lastLines={lastOutputLines[tab.id] ?? []}
                    state={state}
                    onRestart={onRestartInZone ? () => onRestartInZone(zoneIdx) : undefined}
                    onExportZone={onExportZone ? (fmt) => onExportZone(zoneIdx, fmt) : undefined}
                  />
                )}
                {/* Terminal or Plan viewer - always mounted at same tree position to preserve state */}
                {tab.type === "plan" && tab.planFilePath ? (
                  <div className="h-full w-full">
                    <PlanViewer filePath={tab.planFilePath} visible={!useCompact} />
                  </div>
                ) : (
                  <div
                    className={`h-full w-full ${useCompact ? "hidden" : ""}`}
                    style={{
                      paddingTop: useCompact
                        ? undefined
                        : showLabels
                          ? showFilterInput === zoneIdx
                            ? "46px"
                            : "20px"
                          : showFilterInput === zoneIdx
                            ? "26px"
                            : undefined,
                    }}
                  >
                    <TerminalInstance
                      ref={terminalRefs.get(tab.id)}
                      terminalId={tab.id}
                      visible={!useCompact}
                      isReconnecting={tab.isReconnecting}
                      onReconnected={() => onReconnected(tab.id)}
                      onExit={(code) => onExit(tab.id, code)}
                      onFirstInput={(input) => onFirstInput(tab.id, input)}
                      onShellIntegration={(event) => onShellIntegration(tab.id, event)}
                      onOutput={(text) => onOutput(tab.id, text)}
                    />
                  </div>
                )}
              </>
            ) : (
              <div
                className={`h-full w-full flex flex-col items-center justify-center text-xs gap-2 transition-colors ${
                  isDropTarget ? "bg-[#7aa2f7]/5" : "bg-transparent"
                }`}
              >
                <div
                  className={`p-3 rounded-lg border-2 border-dashed transition-colors ${
                    isDropTarget
                      ? "border-[#7aa2f7]/50 text-[#7aa2f7]"
                      : "border-[#2a2d3d] text-[#565f89]"
                  }`}
                >
                  <Terminal className="w-5 h-5 mx-auto mb-1.5 opacity-50" />
                  <span className="text-[10px] font-medium block text-center">
                    Zone {zoneIdx + 1}
                  </span>
                  <span className="text-[9px] opacity-60 block text-center mt-0.5">
                    {isDropTarget ? "Release to assign" : "Drag a tab here"}
                  </span>
                </div>
              </div>
            )}
          </div>
        );
      })}

      {/* Column resize handles */}
      {layout.columns > 1 &&
        Array.from({ length: layout.columns - 1 }, (_, i) => {
          const leftFr = colRatios.slice(0, i + 1).reduce((a, b) => a + b, 0);
          const totalFr = colRatios.reduce((a, b) => a + b, 0);
          const pct = (leftFr / totalFr) * 100;
          return (
            <div
              key={`col-handle-${i}`}
              className="absolute top-0 bottom-0 z-20 cursor-col-resize group"
              style={{ left: `${pct}%`, width: "8px", marginLeft: "-4px" }}
              onDoubleClick={(e) => {
                e.preventDefault();
                setColRatios(Array(layout.columns).fill(1));
              }}
              onMouseDown={(e) => {
                e.preventDefault();
                const startX = e.clientX;
                const gridWidth = gridRef.current?.getBoundingClientRect().width ?? 1;
                const startRatios = [...colRatios];
                const onMove = (ev: MouseEvent) => {
                  const dx = ev.clientX - startX;
                  const dFr = (dx / gridWidth) * totalFr;
                  const newLeft = Math.max(0.15, startRatios[i] + dFr);
                  const newRight = Math.max(0.15, startRatios[i + 1] - dFr);
                  const next = [...startRatios];
                  next[i] = newLeft;
                  next[i + 1] = newRight;
                  setColRatios(next);
                };
                const onUp = () => {
                  document.removeEventListener("mousemove", onMove);
                  document.removeEventListener("mouseup", onUp);
                };
                document.addEventListener("mousemove", onMove);
                document.addEventListener("mouseup", onUp);
              }}
            >
              <div className="w-px h-full mx-auto bg-[#2a2d3d] group-hover:bg-[#7aa2f7] transition-colors" />
            </div>
          );
        })}

      {/* Row resize handles */}
      {layout.rows > 1 &&
        Array.from({ length: layout.rows - 1 }, (_, i) => {
          const topFr = rowRatios.slice(0, i + 1).reduce((a, b) => a + b, 0);
          const totalFr = rowRatios.reduce((a, b) => a + b, 0);
          const pct = (topFr / totalFr) * 100;
          return (
            <div
              key={`row-handle-${i}`}
              className="absolute left-0 right-0 z-20 cursor-row-resize group"
              style={{ top: `${pct}%`, height: "8px", marginTop: "-4px" }}
              onDoubleClick={(e) => {
                e.preventDefault();
                setRowRatios(Array(layout.rows).fill(1));
              }}
              onMouseDown={(e) => {
                e.preventDefault();
                const startY = e.clientY;
                const gridHeight = gridRef.current?.getBoundingClientRect().height ?? 1;
                const startRatios = [...rowRatios];
                const onMove = (ev: MouseEvent) => {
                  const dy = ev.clientY - startY;
                  const dFr = (dy / gridHeight) * totalFr;
                  const newTop = Math.max(0.15, startRatios[i] + dFr);
                  const newBottom = Math.max(0.15, startRatios[i + 1] - dFr);
                  const next = [...startRatios];
                  next[i] = newTop;
                  next[i + 1] = newBottom;
                  setRowRatios(next);
                };
                const onUp = () => {
                  document.removeEventListener("mousemove", onMove);
                  document.removeEventListener("mouseup", onUp);
                };
                document.addEventListener("mousemove", onMove);
                document.addEventListener("mouseup", onUp);
              }}
            >
              <div className="h-px w-full my-auto bg-[#2a2d3d] group-hover:bg-[#7aa2f7] transition-colors" />
            </div>
          );
        })}

      {renderHiddenTabs(unassignedTerminals)}

      {/* Right-click context menu */}
      {contextMenu &&
        (() => {
          const cmTabId = assignments[contextMenu.zoneIndex];
          const cmTab = tabs.find((t) => t.id === cmTabId);
          const cmState = cmTab ? (sessionStates[cmTab.id] ?? "idle") : "idle";
          const others = layout.zones
            .map((_, idx) => {
              if (idx === contextMenu.zoneIndex) return null;
              const otherTabId = assignments[idx];
              const otherTab = tabs.find((t) => t.id === otherTabId);
              return { index: idx, title: otherTab?.title ?? `Zone ${idx + 1}` };
            })
            .filter((z): z is { index: number; title: string } => z !== null);

          return (
            <ZoneContextMenu
              x={contextMenu.x}
              y={contextMenu.y}
              zoneIndex={contextMenu.zoneIndex}
              tab={cmTab}
              state={cmState}
              otherZones={others}
              onClose={() => setContextMenu(null)}
              onFocus={() => onZoneClick(contextMenu.zoneIndex)}
              onMaximize={() => onZoneDoubleClick(contextMenu.zoneIndex)}
              onApprove={() => {
                if (cmTab) {
                  const ref = terminalRefs.get(cmTab.id);
                  ref?.current?.writeToTerminal("y\r");
                }
              }}
              onReject={() => {
                if (cmTab) {
                  const ref = terminalRefs.get(cmTab.id);
                  ref?.current?.writeToTerminal("n\r");
                }
              }}
              onSwap={(targetZone) => {
                if (cmTab && onAssignTab) {
                  // Swap by assigning this tab to the target zone
                  // The assignTabToZone function handles the swap logic
                  onAssignTab(targetZone, cmTab.id);
                }
              }}
              onUnassign={() => {
                if (onAssignTab) {
                  // Assign empty string to effectively unassign
                  // We need to remove the zone assignment - use a special value
                  // Actually, let's swap with an unassigned tab or just reassign
                  const unassigned = tabs.find((t) => !Object.values(assignments).includes(t.id));
                  if (unassigned) {
                    onAssignTab(contextMenu.zoneIndex, unassigned.id);
                  }
                }
              }}
              onRestart={onRestartInZone ? () => onRestartInZone(contextMenu.zoneIndex) : undefined}
            />
          );
        })()}
    </div>
  );
}
