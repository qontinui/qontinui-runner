import { useState, useRef, useEffect, useMemo, type ReactNode } from "react";
import { Terminal, X, Plus, List, ChevronDown } from "lucide-react";
import type { TerminalTab } from "./useTerminalManager";
import type { SessionState, ZoneAssignments } from "./useZoneLayout";

interface TerminalTabBarProps {
  tabs: TerminalTab[];
  activeId: string | null;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
  onCreate: () => void;
  onRename: (id: string, title: string) => void;
  sessionStates?: Record<string, SessionState>;
  layoutPicker?: ReactNode;
  onQuickLaunch?: (count: number, autoCommand?: string) => void;
  // Zone monitoring enrichments
  activityData?: Record<string, number[]>;
  stateDurations?: Record<string, string>;
  lastOutputLines?: Record<string, string[]>;
  unreadTabs?: Set<string>;
  staleTabs?: Set<string>;
  zoneLabels?: Record<number, string>;
  labelColorMap?: Record<string, string>;
  assignments?: ZoneAssignments;
}

const STATE_DOT_COLORS: Record<SessionState, string> = {
  idle: "bg-[#565f89]",
  working: "bg-[#7aa2f7]",
  "needs-input": "bg-[#e0af68]",
  completed: "bg-[#9ece6a]",
  error: "bg-[#f7768e]",
};

const STATE_BAR_COLORS: Record<SessionState, string> = {
  idle: "#565f89",
  working: "#7aa2f7",
  "needs-input": "#e0af68",
  completed: "#9ece6a",
  error: "#f7768e",
};

function formatTime(value?: string | number): string {
  if (value === undefined || value === null) return "";
  try {
    const d = new Date(value);
    return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  } catch {
    return "";
  }
}

// ── Activity Sparkline ─────────────────────────────────────────────────────

function ActivitySparkline({ data, color }: { data: number[]; color: string }) {
  const points = data.slice(-8);
  if (points.length === 0) return null;
  const max = Math.max(...points, 1);
  const barWidth = 30 / points.length;

  return (
    <svg width={30} height={10} viewBox="0 0 30 10" className="shrink-0 opacity-80">
      {points.map((v, i) => {
        const h = Math.max((v / max) * 9, 0.5);
        return (
          <rect
            key={i}
            x={i * barWidth + 0.5}
            y={10 - h}
            width={Math.max(barWidth - 1, 1)}
            height={h}
            fill={color}
            rx={0.5}
          />
        );
      })}
    </svg>
  );
}

// ── Tag Badges ─────────────────────────────────────────────────────────────

function TagBadges({
  labels,
  colorMap,
  max = 2,
}: {
  labels: string;
  colorMap?: Record<string, string>;
  max?: number;
}) {
  const tags = labels
    .split(",")
    .map((t) => t.trim())
    .filter(Boolean);
  if (tags.length === 0) return null;

  const visible = tags.slice(0, max);
  const overflow = tags.length - max;

  return (
    <span className="flex items-center gap-0.5 flex-wrap">
      {visible.map((tag) => (
        <span
          key={tag}
          className="px-1 py-0 rounded text-[8px] leading-[14px] font-medium"
          style={{
            backgroundColor: (colorMap?.[tag] ?? "#565f89") + "22",
            color: colorMap?.[tag] ?? "#a9b1d6",
          }}
        >
          {tag}
        </span>
      ))}
      {overflow > 0 && <span className="text-[8px] text-[#565f89]">+{overflow}</span>}
    </span>
  );
}

// ── Duration Badge ─────────────────────────────────────────────────────────

function DurationBadge({ duration, state }: { duration: string; state: SessionState }) {
  if (!duration) return null;
  const show = state === "needs-input" || state === "error" || state === "working";
  if (!show) return null;

  const color =
    state === "error"
      ? "text-[#f7768e]"
      : state === "needs-input"
        ? "text-[#e0af68]"
        : "text-[#7aa2f7]";

  return <span className={`text-[9px] ${color} opacity-80 shrink-0`}>{duration}</span>;
}

// ── Main Component ─────────────────────────────────────────────────────────

export function TerminalTabBar({
  tabs,
  activeId,
  onSelect,
  onClose,
  onCreate,
  onRename,
  sessionStates,
  layoutPicker,
  onQuickLaunch,
  activityData,
  stateDurations,
  unreadTabs,
  staleTabs,
  zoneLabels,
  labelColorMap,
  assignments,
}: TerminalTabBarProps) {
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editValue, setEditValue] = useState("");
  const [showDropdown, setShowDropdown] = useState(false);
  const [showQuickLaunch, setShowQuickLaunch] = useState(false);
  const [quickLaunchCmd, setQuickLaunchCmd] = useState("");
  const dropdownRef = useRef<HTMLDivElement>(null);
  const quickLaunchRef = useRef<HTMLDivElement>(null);

  // Live elapsed-time tick (30s interval), only in multi-zone mode
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!assignments) return;
    const id = setInterval(() => setTick((t) => t + 1), 30_000);
    return () => clearInterval(id);
  }, [assignments]);

  // Reverse lookup: tab ID -> zone index
  const tabIdToZone = useMemo<Record<string, number>>(() => {
    if (!assignments) return {};
    const map: Record<string, number> = {};
    for (const [zoneStr, tabId] of Object.entries(assignments)) {
      map[tabId] = Number(zoneStr);
    }
    return map;
  }, [assignments]);

  const isMultiZone = !!assignments;

  const startEditing = (tab: TerminalTab) => {
    setEditingId(tab.id);
    setEditValue(tab.title);
  };

  const commitEdit = () => {
    if (editingId && editValue.trim()) {
      onRename(editingId, editValue.trim());
    }
    setEditingId(null);
  };

  // Close dropdowns when clicking outside
  useEffect(() => {
    if (!showDropdown && !showQuickLaunch) return;
    const handleClick = (e: MouseEvent) => {
      if (showDropdown && dropdownRef.current && !dropdownRef.current.contains(e.target as Node)) {
        setShowDropdown(false);
      }
      if (
        showQuickLaunch &&
        quickLaunchRef.current &&
        !quickLaunchRef.current.contains(e.target as Node)
      ) {
        setShowQuickLaunch(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [showDropdown, showQuickLaunch]);

  // ── Helpers for per-tab enrichments ──────────────────────────────────────

  function getTabZoneIndex(tabId: string): number | undefined {
    return tabIdToZone[tabId];
  }

  function getTabLabels(tabId: string): string | undefined {
    const zone = getTabZoneIndex(tabId);
    if (zone === undefined) return undefined;
    return zoneLabels?.[zone];
  }

  function isTabAssigned(tabId: string): boolean {
    return tabId in tabIdToZone;
  }

  // ── Render ───────────────────────────────────────────────────────────────

  return (
    <div className="flex items-center gap-0.5 bg-[#13141f] border-b border-[#2a2d3d] px-1 h-9 shrink-0 overflow-x-auto scrollbar-none">
      {/* Layout picker */}
      {layoutPicker && <div className="shrink-0 mr-1">{layoutPicker}</div>}

      {tabs.map((tab) => {
        const isActive = tab.id === activeId;
        const isDead = !tab.isAlive;
        const isEditing = editingId === tab.id;
        const state = sessionStates?.[tab.id] ?? "idle";
        const hasUnread = unreadTabs?.has(tab.id) ?? false;
        const isStale = staleTabs?.has(tab.id) ?? false;
        const tabActivity = activityData?.[tab.id];
        const tabDuration = stateDurations?.[tab.id];
        const tabLabels = getTabLabels(tab.id);
        const showSparkline =
          isMultiZone && isTabAssigned(tab.id) && tabActivity && tabActivity.length > 0;

        return (
          <button
            key={tab.id}
            onClick={() => onSelect(tab.id)}
            onDoubleClick={() => startEditing(tab)}
            onAuxClick={(e) => {
              if (e.button === 1) {
                e.preventDefault();
                onClose(tab.id);
              }
            }}
            draggable
            onDragStart={(e) => {
              e.dataTransfer.setData("text/tab-id", tab.id);
              e.dataTransfer.effectAllowed = "move";
            }}
            className={`
              flex items-center gap-1.5 px-3 py-1 rounded-t text-xs font-medium
              transition-colors whitespace-nowrap max-w-[260px] group cursor-grab active:cursor-grabbing
              ${
                isActive
                  ? "bg-[#1a1b26] text-[#c0caf5] border-t border-x border-[#2a2d3d] -mb-px"
                  : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#1a1b26]/50"
              }
              ${isDead ? "opacity-60" : ""}
              ${isStale ? "border-dashed border-[#565f89]/40" : ""}
            `}
          >
            {/* Session state dot */}
            {sessionStates ? (
              <div
                className={`w-2 h-2 rounded-full shrink-0 ${STATE_DOT_COLORS[state]} ${
                  state === "needs-input" ? "animate-pulse" : ""
                }`}
              />
            ) : (
              <Terminal className="w-3 h-3 shrink-0" />
            )}

            <span className="flex flex-col items-start min-w-0">
              {/* Title row */}
              <span className="flex items-center gap-1 min-w-0">
                {isEditing ? (
                  <input
                    autoFocus
                    value={editValue}
                    onChange={(e) => setEditValue(e.target.value)}
                    onBlur={commitEdit}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") commitEdit();
                      if (e.key === "Escape") setEditingId(null);
                      e.stopPropagation();
                    }}
                    onClick={(e) => e.stopPropagation()}
                    className="bg-transparent border-b border-[#565f89] text-[#c0caf5] text-xs w-20 outline-none"
                  />
                ) : (
                  <span className="truncate">{tab.title}</span>
                )}

                {/* Inline sparkline + duration (active tab only) */}
                {isActive && showSparkline && (
                  <ActivitySparkline data={tabActivity!} color={STATE_BAR_COLORS[state]} />
                )}
                {isActive && tabDuration && <DurationBadge duration={tabDuration} state={state} />}
              </span>

              {/* Tags row (active tab only, multi-zone) */}
              {isActive && tabLabels && isMultiZone && (
                <TagBadges labels={tabLabels} colorMap={labelColorMap} />
              )}

              {/* Working dir (active tab only) */}
              {isActive && tab.workingDir && (
                <span className="text-[9px] text-[#565f89] truncate max-w-[140px]">
                  {tab.workingDir.split(/[/\\]/).slice(-2).join("/")}
                </span>
              )}
            </span>

            {/* Stale indicator (inactive tabs) */}
            {!isActive && isStale && (
              <span className="text-[8px] text-[#565f89] italic shrink-0">stalled?</span>
            )}

            {/* Duration badge on inactive tabs for attention states */}
            {!isActive && tabDuration && <DurationBadge duration={tabDuration} state={state} />}

            {/* Exit code badge */}
            {isDead && tab.exitCode !== null && (
              <span
                className={`text-[10px] px-1 rounded ${
                  tab.exitCode === 0
                    ? "bg-green-900/30 text-green-400"
                    : "bg-red-900/30 text-red-400"
                }`}
              >
                {tab.exitCode}
              </span>
            )}

            {/* Unread indicator */}
            {hasUnread && <span className="w-1.5 h-1.5 rounded-full bg-[#f7768e] shrink-0" />}

            {/* Close button */}
            <span
              role="button"
              tabIndex={0}
              onClick={(e) => {
                e.stopPropagation();
                onClose(tab.id);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.stopPropagation();
                  onClose(tab.id);
                }
              }}
              className="ml-1 p-0.5 rounded opacity-0 group-hover:opacity-100 hover:bg-[#2a2d3d] transition-opacity"
            >
              <X className="w-3 h-3" />
            </span>
          </button>
        );
      })}

      <button
        onClick={onCreate}
        className="flex items-center justify-center w-6 h-6 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#1a1b26]/50 transition-colors ml-1 shrink-0"
        title="New terminal (Ctrl+Shift+T)"
      >
        <Plus className="w-3.5 h-3.5" />
      </button>

      {/* Quick-launch dropdown */}
      {onQuickLaunch && (
        <div className="relative shrink-0" ref={quickLaunchRef}>
          <button
            onClick={() => setShowQuickLaunch(!showQuickLaunch)}
            className={`flex items-center justify-center w-5 h-6 rounded transition-colors ${
              showQuickLaunch
                ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
                : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#1a1b26]/50"
            }`}
            title="Launch multiple terminals"
          >
            <ChevronDown className="w-3 h-3" />
          </button>
          {showQuickLaunch && (
            <div className="absolute left-0 top-full mt-1 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden min-w-[200px]">
              <div className="px-3 py-1.5 text-[9px] uppercase tracking-wider text-[#565f89] border-b border-[#2a2d3d]">
                Quick Launch
              </div>
              <div className="px-3 py-1.5 border-b border-[#2a2d3d]">
                <input
                  value={quickLaunchCmd}
                  onChange={(e) => setQuickLaunchCmd(e.target.value)}
                  onKeyDown={(e) => e.stopPropagation()}
                  placeholder="Auto-run command (e.g. claude)"
                  className="w-full bg-[#13141f] border border-[#2a2d3d] rounded px-2 py-1 text-[10px] text-[#c0caf5] placeholder-[#565f89] outline-none focus:border-[#7aa2f7] transition-colors"
                />
              </div>
              {[2, 4, 6, 9].map((count) => (
                <button
                  key={count}
                  onClick={() => {
                    onQuickLaunch(count, quickLaunchCmd.trim() || undefined);
                    setShowQuickLaunch(false);
                  }}
                  className="w-full text-left px-3 py-1.5 text-[11px] text-[#c0caf5] hover:bg-[#7aa2f7]/10 transition-colors"
                >
                  Launch {count} terminals
                  {quickLaunchCmd.trim() ? ` + "${quickLaunchCmd.trim()}"` : ""}
                </button>
              ))}
            </div>
          )}
        </div>
      )}

      {/* Session dropdown */}
      {tabs.length > 0 && (
        <div className="relative ml-auto shrink-0" ref={dropdownRef}>
          <button
            onClick={() => setShowDropdown(!showDropdown)}
            className={`flex items-center justify-center w-6 h-6 rounded transition-colors ${
              showDropdown
                ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
                : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#1a1b26]/50"
            }`}
            title="All sessions"
          >
            <List className="w-3.5 h-3.5" />
          </button>

          {showDropdown && (
            <div className="absolute right-0 top-full mt-1 w-80 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden">
              <div className="px-3 py-2 text-[10px] uppercase tracking-wider text-[#565f89] border-b border-[#2a2d3d]">
                Sessions ({tabs.length})
              </div>
              <div className="max-h-64 overflow-y-auto scrollbar-dark">
                {tabs.map((tab) => {
                  const isActive = tab.id === activeId;
                  const isDead = !tab.isAlive;
                  const state = sessionStates?.[tab.id] ?? "idle";
                  const tabActivity = activityData?.[tab.id];
                  const tabDuration = stateDurations?.[tab.id];
                  const tabLabels = getTabLabels(tab.id);
                  const hasSparkline =
                    isMultiZone && isTabAssigned(tab.id) && tabActivity && tabActivity.length > 0;

                  return (
                    <div
                      key={tab.id}
                      onClick={() => {
                        onSelect(tab.id);
                        setShowDropdown(false);
                      }}
                      className={`flex items-start gap-2 px-3 py-2 cursor-pointer transition-colors ${
                        isActive ? "bg-[#7aa2f7]/10" : "hover:bg-[#2a2d3d]/50"
                      }`}
                    >
                      {/* Status dot */}
                      <div className="mt-1 shrink-0">
                        <div
                          className={`w-2 h-2 rounded-full ${
                            sessionStates
                              ? `${STATE_DOT_COLORS[state]} ${state === "needs-input" ? "animate-pulse" : ""}`
                              : isDead
                                ? "bg-[#565f89]"
                                : "bg-[#9ece6a]"
                          }`}
                        />
                      </div>

                      {/* Info */}
                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-1.5">
                          <span
                            className={`text-xs font-medium truncate ${
                              isActive ? "text-[#7aa2f7]" : "text-[#c0caf5]"
                            }`}
                          >
                            {tab.title}
                          </span>
                          {isDead && tab.exitCode !== null && (
                            <span
                              className={`text-[10px] px-1 rounded ${
                                tab.exitCode === 0
                                  ? "bg-green-900/30 text-green-400"
                                  : "bg-red-900/30 text-red-400"
                              }`}
                            >
                              exit {tab.exitCode}
                            </span>
                          )}
                          {/* Sparkline in dropdown */}
                          {hasSparkline && (
                            <ActivitySparkline
                              data={tabActivity!}
                              color={STATE_BAR_COLORS[state]}
                            />
                          )}
                          {/* Duration in dropdown */}
                          {tabDuration && <DurationBadge duration={tabDuration} state={state} />}
                        </div>
                        <div className="flex items-center gap-2 text-[10px] text-[#565f89] mt-0.5">
                          {tab.pid && <span>PID {tab.pid}</span>}
                          {tab.workingDir && (
                            <span className="truncate" title={tab.workingDir}>
                              {tab.workingDir.split(/[/\\]/).pop()}
                            </span>
                          )}
                          {tab.createdAt && <span>{formatTime(tab.createdAt)}</span>}
                        </div>
                        {/* Tag badges in dropdown */}
                        {tabLabels && isMultiZone && (
                          <div className="mt-0.5">
                            <TagBadges labels={tabLabels} colorMap={labelColorMap} max={3} />
                          </div>
                        )}
                      </div>

                      {/* Close button */}
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          onClose(tab.id);
                        }}
                        className="mt-0.5 p-0.5 rounded text-[#565f89] hover:text-[#f7768e] hover:bg-[#f7768e]/10 transition-colors shrink-0"
                      >
                        <X className="w-3 h-3" />
                      </button>
                    </div>
                  );
                })}
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
